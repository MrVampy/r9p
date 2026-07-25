use super::direct::DirectClient;
use crate::{AuthorityBindings, ConnectionConfig, Error, RequestTracker, Result};
use r9p::{
    fid::Fid, multiplex::DelimitedRead, qid::Qid, referral::NamespaceReferral, stat::Stat, Variant,
    NOFID,
};
use std::{
    collections::BTreeMap,
    str,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const ROOT_FID: Fid = 1;
const FIRST_DYNAMIC_FID: Fid = ROOT_FID + 1;
const DEFAULT_REFERRAL_TIMEOUT: Duration = Duration::from_secs(5);

/// One logical 9P namespace.
///
/// A client attaches to the admitted root once. When that root advertises
/// namespace referrals through 9P2000.R, this type establishes and reuses the
/// direct sessions internally. Callers continue to operate on ordinary fids.
#[derive(Clone)]
pub struct Client {
    state: Arc<State>,
}

struct State {
    root: DirectClient,
    tracker: RequestTracker,
    authorities: AuthorityBindings,
    service_msize: u32,
    connect_timeout: Duration,
    routes: Mutex<Vec<Arc<Route>>>,
    connected_routes: Mutex<Vec<DirectClient>>,
    fids: Mutex<FidTable>,
}

struct Route {
    referral: NamespaceReferral,
    received_at: Instant,
    client: Mutex<Option<DirectClient>>,
}

#[derive(Clone)]
struct FidBinding {
    client: DirectClient,
    remote_fid: Fid,
    namespace_path: Vec<Vec<u8>>,
    route_mount: Option<Vec<u8>>,
}

struct FidTable {
    next: Fid,
    bindings: BTreeMap<Fid, FidBinding>,
}

struct RoutedTarget {
    client: DirectClient,
    remote_path: Vec<Vec<u8>>,
    route_mount: Option<Vec<u8>>,
}

impl Client {
    pub fn connect_with_timeout(config: &ConnectionConfig, timeout: Duration) -> Result<Self> {
        Self::connect_with_tracker_timeout(config, RequestTracker::default(), timeout)
    }

    pub fn connect_with_tracker(
        config: &ConnectionConfig,
        tracker: RequestTracker,
    ) -> Result<Self> {
        Self::connect_with_tracker_timeout(config, tracker, Duration::ZERO)
    }

    pub fn connect_with_tracker_timeout(
        config: &ConnectionConfig,
        tracker: RequestTracker,
        timeout: Duration,
    ) -> Result<Self> {
        let root = DirectClient::connect_with_tracker_timeout(config, tracker.clone(), timeout)?;
        let referral_timeout = bounded_referral_timeout(timeout);
        let referrals = if root.variant().supports_referrals() {
            root.referrals_timeout(referral_timeout)?
        } else {
            Vec::new()
        };
        let routes = build_routes(referrals, &[])?;
        let root_binding = FidBinding {
            client: root.clone(),
            remote_fid: root.root_fid(),
            namespace_path: Vec::new(),
            route_mount: None,
        };
        let mut bindings = BTreeMap::new();
        bindings.insert(ROOT_FID, root_binding);
        Ok(Self {
            state: Arc::new(State {
                root,
                tracker,
                authorities: config.authorities.clone(),
                service_msize: config.msize,
                connect_timeout: timeout,
                routes: Mutex::new(routes),
                connected_routes: Mutex::new(Vec::new()),
                fids: Mutex::new(FidTable {
                    next: FIRST_DYNAMIC_FID,
                    bindings,
                }),
            }),
        })
    }

    pub fn tracker(&self) -> RequestTracker {
        self.state.tracker.clone()
    }

    pub fn interrupt_fuse_unique(&self, unique: u64, timeout: Duration) -> Result<usize> {
        self.state.tracker.interrupt(unique, timeout)
    }

    pub const fn root_fid(&self) -> Fid {
        ROOT_FID
    }

    pub fn variant(&self) -> Variant {
        self.state.root.variant()
    }

    pub fn msize(&self) -> u32 {
        self.state.root.msize()
    }

    pub fn version(&self) -> Vec<u8> {
        self.state.root.version()
    }

    pub fn root_qid(&self) -> Qid {
        self.state.root.root_qid()
    }

    pub fn max_write_payload(&self) -> u32 {
        self.state.root.max_write_payload()
    }

    pub fn clone_fid(&self, fid: Fid) -> Result<Fid> {
        let binding = self.binding(fid)?;
        let remote_fid = binding.client.clone_fid(binding.remote_fid)?;
        self.allocate_binding(FidBinding {
            remote_fid,
            ..binding
        })
    }

    pub fn clone_fid_timeout(&self, fid: Fid, timeout: Duration) -> Result<Fid> {
        let binding = self.binding(fid)?;
        let remote_fid = binding
            .client
            .clone_fid_timeout(binding.remote_fid, timeout)?;
        self.allocate_binding(FidBinding {
            remote_fid,
            ..binding
        })
    }

    pub fn walk_one_timeout(&self, fid: Fid, name: &[u8], timeout: Duration) -> Result<Fid> {
        self.walk_timeout(fid, &[name.to_vec()], timeout)
    }

    pub fn walk_timeout(&self, fid: Fid, names: &[Vec<u8>], timeout: Duration) -> Result<Fid> {
        let binding = self.binding(fid)?;
        let namespace_path = apply_walk(&binding.namespace_path, names)?;
        let (target, remote_fid) = self.walk_namespace_path_timeout(&namespace_path, timeout)?;
        self.allocate_binding(FidBinding {
            client: target.client,
            remote_fid,
            namespace_path,
            route_mount: target.route_mount,
        })
    }

    pub fn walk_path(&self, path: &str) -> Result<Fid> {
        let namespace_path = parse_namespace_path(path.as_bytes())?;
        let (target, remote_fid) = self.walk_namespace_path(&namespace_path)?;
        self.allocate_binding(FidBinding {
            client: target.client,
            remote_fid,
            namespace_path,
            route_mount: target.route_mount,
        })
    }

    pub fn walk_path_timeout(&self, path: &str, timeout: Duration) -> Result<Fid> {
        let namespace_path = parse_namespace_path(path.as_bytes())?;
        let (target, remote_fid) = self.walk_namespace_path_timeout(&namespace_path, timeout)?;
        self.allocate_binding(FidBinding {
            client: target.client,
            remote_fid,
            namespace_path,
            route_mount: target.route_mount,
        })
    }

    pub fn open(&self, fid: Fid, mode: u8) -> Result<Qid> {
        let binding = self.binding(fid)?;
        binding.client.open(binding.remote_fid, mode)
    }

    pub fn open_timeout(&self, fid: Fid, mode: u8, timeout: Duration) -> Result<Qid> {
        let binding = self.binding(fid)?;
        binding
            .client
            .open_timeout(binding.remote_fid, mode, timeout)
    }

    pub fn create_timeout(
        &self,
        parent_fid: Fid,
        name: &[u8],
        perm: u32,
        mode: u8,
        timeout: Duration,
    ) -> Result<(Fid, Qid)> {
        validate_path_element(name)?;
        let parent = self.binding(parent_fid)?;
        let mut namespace_path = parent.namespace_path.clone();
        namespace_path.push(name.to_vec());
        let selected = self.routed_target(&namespace_path, timeout)?;
        if selected.route_mount != parent.route_mount {
            return Err(Error::new(
                libc::EXDEV,
                "create cannot cross a namespace referral boundary",
            ));
        }
        let (remote_fid, qid) =
            parent
                .client
                .create_timeout(parent.remote_fid, name, perm, mode, timeout)?;
        let fid = self.allocate_binding(FidBinding {
            client: parent.client,
            remote_fid,
            namespace_path,
            route_mount: parent.route_mount,
        })?;
        Ok((fid, qid))
    }

    pub fn create(&self, parent_fid: Fid, name: &[u8], perm: u32, mode: u8) -> Result<(Fid, Qid)> {
        validate_path_element(name)?;
        let parent = self.binding(parent_fid)?;
        let mut namespace_path = parent.namespace_path.clone();
        namespace_path.push(name.to_vec());
        let selected = self.routed_target(&namespace_path, Duration::ZERO)?;
        if selected.route_mount != parent.route_mount {
            return Err(Error::new(
                libc::EXDEV,
                "create cannot cross a namespace referral boundary",
            ));
        }
        let (remote_fid, qid) = parent.client.create(parent.remote_fid, name, perm, mode)?;
        let fid = self.allocate_binding(FidBinding {
            client: parent.client,
            remote_fid,
            namespace_path,
            route_mount: parent.route_mount,
        })?;
        Ok((fid, qid))
    }

    pub fn read_timeout(
        &self,
        fid: Fid,
        offset: u64,
        count: u32,
        timeout: Duration,
    ) -> Result<Vec<u8>> {
        let binding = self.binding(fid)?;
        binding
            .client
            .read_timeout(binding.remote_fid, offset, count, timeout)
    }

    pub fn read(&self, fid: Fid, offset: u64, count: u32) -> Result<Vec<u8>> {
        let binding = self.binding(fid)?;
        binding.client.read(binding.remote_fid, offset, count)
    }

    pub fn read_full_timeout(
        &self,
        fid: Fid,
        offset: u64,
        count: u32,
        timeout: Duration,
    ) -> Result<Vec<u8>> {
        let binding = self.binding(fid)?;
        binding
            .client
            .read_full_timeout(binding.remote_fid, offset, count, timeout)
    }

    pub fn read_delimited_timeout(
        &self,
        fid: Fid,
        offset: u64,
        count: u32,
        delimiter: u8,
        timeout: Duration,
    ) -> Result<Vec<u8>> {
        let binding = self.binding(fid)?;
        binding
            .client
            .read_delimited_timeout(binding.remote_fid, offset, count, delimiter, timeout)
    }

    pub fn read_full(&self, fid: Fid, offset: u64, count: u32) -> Result<Vec<u8>> {
        let binding = self.binding(fid)?;
        binding.client.read_full(binding.remote_fid, offset, count)
    }

    pub fn read_delimited(
        &self,
        fid: Fid,
        offset: u64,
        count: u32,
        delimiter: u8,
    ) -> Result<Vec<u8>> {
        let binding = self.binding(fid)?;
        binding
            .client
            .read_delimited(binding.remote_fid, offset, count, delimiter)
    }

    pub fn write_timeout(
        &self,
        fid: Fid,
        offset: u64,
        data: &[u8],
        timeout: Duration,
    ) -> Result<u32> {
        let binding = self.binding(fid)?;
        binding
            .client
            .write_timeout(binding.remote_fid, offset, data, timeout)
    }

    pub fn write(&self, fid: Fid, offset: u64, data: &[u8]) -> Result<u32> {
        let binding = self.binding(fid)?;
        binding.client.write(binding.remote_fid, offset, data)
    }

    pub fn write_once(&self, fid: Fid, offset: u64, data: &[u8]) -> Result<u32> {
        let binding = self.binding(fid)?;
        binding.client.write_once(binding.remote_fid, offset, data)
    }

    pub fn write_once_timeout(
        &self,
        fid: Fid,
        offset: u64,
        data: &[u8],
        timeout: Duration,
    ) -> Result<u32> {
        let binding = self.binding(fid)?;
        binding
            .client
            .write_once_timeout(binding.remote_fid, offset, data, timeout)
    }

    pub fn write_then_read_delimited_timeout(
        &self,
        fid: Fid,
        write_offset: u64,
        data: &[u8],
        read: DelimitedRead,
        timeout: Duration,
    ) -> Result<(u32, Vec<u8>)> {
        let binding = self.binding(fid)?;
        binding.client.write_then_read_delimited_timeout(
            binding.remote_fid,
            write_offset,
            data,
            read,
            timeout,
        )
    }

    pub fn clunk_timeout(&self, fid: Fid, timeout: Duration) -> Result<()> {
        if fid == ROOT_FID {
            return Err(Error::new(libc::EBUSY, "cannot clunk the namespace root"));
        }
        let binding = self.binding(fid)?;
        binding.client.clunk_timeout(binding.remote_fid, timeout)?;
        self.remove_binding(fid)
    }

    pub fn clunk(&self, fid: Fid) -> Result<()> {
        if fid == ROOT_FID {
            return Err(Error::new(libc::EBUSY, "cannot clunk the namespace root"));
        }
        let binding = self.binding(fid)?;
        binding.client.clunk(binding.remote_fid)?;
        self.remove_binding(fid)
    }

    pub fn shutdown(&self) -> Result<()> {
        let routes = self
            .state
            .connected_routes
            .lock()
            .map_err(|_| Error::new(libc::EIO, "namespace route lock poisoned"))?
            .clone();
        let mut first_error = None;
        for route in routes {
            if let Err(error) = route.shutdown() {
                first_error.get_or_insert(error);
            }
        }
        if let Err(error) = self.state.root.shutdown() {
            first_error.get_or_insert(error);
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    pub fn remove_timeout(&self, fid: Fid, timeout: Duration) -> Result<()> {
        if fid == ROOT_FID {
            return Err(Error::new(libc::EBUSY, "cannot remove the namespace root"));
        }
        let binding = self.binding(fid)?;
        binding.client.remove_timeout(binding.remote_fid, timeout)?;
        self.remove_binding(fid)
    }

    pub fn remove(&self, fid: Fid) -> Result<()> {
        if fid == ROOT_FID {
            return Err(Error::new(libc::EBUSY, "cannot remove the namespace root"));
        }
        let binding = self.binding(fid)?;
        binding.client.remove(binding.remote_fid)?;
        self.remove_binding(fid)
    }

    pub fn stat(&self, fid: Fid) -> Result<Stat> {
        let binding = self.binding(fid)?;
        binding.client.stat(binding.remote_fid)
    }

    pub fn stat_timeout(&self, fid: Fid, timeout: Duration) -> Result<Stat> {
        let binding = self.binding(fid)?;
        binding.client.stat_timeout(binding.remote_fid, timeout)
    }

    pub fn wstat_timeout(&self, fid: Fid, stat: Stat, timeout: Duration) -> Result<()> {
        let binding = self.binding(fid)?;
        binding
            .client
            .wstat_timeout(binding.remote_fid, stat, timeout)
    }

    pub fn wstat(&self, fid: Fid, stat: Stat) -> Result<()> {
        let binding = self.binding(fid)?;
        binding.client.wstat(binding.remote_fid, stat)
    }

    pub(crate) fn validate_stat(&self, stat: Stat) -> Result<Stat> {
        if !self.variant().supports_symlinks()
            && (stat.qid.is_symlink() || stat.mode & r9p::qid::DMSYMLINK != 0)
        {
            return Err(Error::new(
                libc::EPROTO,
                "server exposed symlink metadata without negotiating 9P2000.R",
            ));
        }
        Ok(stat)
    }

    fn binding(&self, fid: Fid) -> Result<FidBinding> {
        self.state
            .fids
            .lock()
            .map_err(|_| Error::new(libc::EIO, "namespace fid lock poisoned"))?
            .bindings
            .get(&fid)
            .cloned()
            .ok_or_else(|| Error::new(libc::EBADF, format!("unknown namespace fid {fid}")))
    }

    fn allocate_binding(&self, binding: FidBinding) -> Result<Fid> {
        let mut fids = self
            .state
            .fids
            .lock()
            .map_err(|_| Error::new(libc::EIO, "namespace fid lock poisoned"))?;
        let start = fids.next;
        loop {
            let candidate = fids.next;
            fids.next = next_fid(candidate);
            if candidate != NOFID && !fids.bindings.contains_key(&candidate) {
                fids.bindings.insert(candidate, binding);
                return Ok(candidate);
            }
            if fids.next == start {
                return Err(Error::new(libc::EMFILE, "namespace fid space exhausted"));
            }
        }
    }

    fn remove_binding(&self, fid: Fid) -> Result<()> {
        self.state
            .fids
            .lock()
            .map_err(|_| Error::new(libc::EIO, "namespace fid lock poisoned"))?
            .bindings
            .remove(&fid)
            .map(|_| ())
            .ok_or_else(|| Error::new(libc::EBADF, format!("unknown namespace fid {fid}")))
    }

    fn walk_namespace_path(&self, namespace_path: &[Vec<u8>]) -> Result<(RoutedTarget, Fid)> {
        let target = self.routed_target(namespace_path, self.state.connect_timeout)?;
        match walk_remote(&target) {
            Ok(fid) => Ok((target, fid)),
            Err(error) if self.should_refresh_routes_after(&error) => {
                self.refresh_routes(self.state.connect_timeout)?;
                let target = self.routed_target(namespace_path, self.state.connect_timeout)?;
                let fid = walk_remote(&target)?;
                Ok((target, fid))
            }
            Err(error) => Err(error),
        }
    }

    fn walk_namespace_path_timeout(
        &self,
        namespace_path: &[Vec<u8>],
        timeout: Duration,
    ) -> Result<(RoutedTarget, Fid)> {
        let target = self.routed_target(namespace_path, timeout)?;
        match walk_remote_timeout(&target, timeout) {
            Ok(fid) => Ok((target, fid)),
            Err(error) if self.should_refresh_routes_after(&error) => {
                self.refresh_routes(timeout)?;
                let target = self.routed_target(namespace_path, timeout)?;
                let fid = walk_remote_timeout(&target, timeout)?;
                Ok((target, fid))
            }
            Err(error) => Err(error),
        }
    }

    fn should_refresh_routes_after(&self, error: &Error) -> bool {
        self.variant().supports_referrals()
            && matches!(
                error.errno,
                libc::ENOENT
                    | libc::ESTALE
                    | libc::ENOTCONN
                    | libc::ECONNABORTED
                    | libc::ECONNRESET
                    | libc::EPIPE
            )
    }

    fn routed_target(&self, namespace_path: &[Vec<u8>], timeout: Duration) -> Result<RoutedTarget> {
        let path = render_namespace_path(namespace_path);
        let mut route = self.find_route(&path)?;
        if route
            .as_ref()
            .is_some_and(|route| !route.has_client() && !route.is_valid())
        {
            self.refresh_routes(timeout)?;
            route = self.find_route(&path)?;
        }
        let Some(route) = route else {
            return Ok(RoutedTarget {
                client: self.state.root.clone(),
                remote_path: namespace_path.to_vec(),
                route_mount: None,
            });
        };
        let client = self.connect_route(&route, timeout)?;
        let remote_path = route
            .referral
            .routed_path(&path)
            .map_err(protocol_error)
            .and_then(|path| parse_namespace_path(&path))?;
        Ok(RoutedTarget {
            client,
            remote_path,
            route_mount: Some(route.referral.mount_path.clone()),
        })
    }

    fn find_route(&self, path: &[u8]) -> Result<Option<Arc<Route>>> {
        let routes = self
            .state
            .routes
            .lock()
            .map_err(|_| Error::new(libc::EIO, "namespace route lock poisoned"))?;
        Ok(routes
            .iter()
            .find(|route| mounted_suffix(path, &route.referral.mount_path).is_some())
            .cloned())
    }

    fn refresh_routes(&self, timeout: Duration) -> Result<()> {
        let referrals = self
            .state
            .root
            .referrals_timeout(bounded_referral_timeout(timeout))?;
        let old_routes = self
            .state
            .routes
            .lock()
            .map_err(|_| Error::new(libc::EIO, "namespace route lock poisoned"))?
            .clone();
        let routes = build_routes(referrals, &old_routes)?;
        *self
            .state
            .routes
            .lock()
            .map_err(|_| Error::new(libc::EIO, "namespace route lock poisoned"))? = routes;
        Ok(())
    }

    fn connect_route(&self, route: &Arc<Route>, timeout: Duration) -> Result<DirectClient> {
        let mut client = route
            .client
            .lock()
            .map_err(|_| Error::new(libc::EIO, "namespace route client lock poisoned"))?;
        if let Some(client) = client.as_ref() {
            return Ok(client.clone());
        }
        if !route.is_valid() {
            return Err(Error::new(
                libc::ESTALE,
                format!(
                    "namespace referral expired before direct session establishment: {}",
                    String::from_utf8_lossy(&route.referral.mount_path)
                ),
            ));
        }
        let authority = text_field("authority_boundary", &route.referral.authority_boundary)?;
        let config = ConnectionConfig {
            address: text_field("endpoint", &route.referral.endpoint)?,
            uname: text_field("uname", &route.referral.uname)?,
            aname: text_field("aname", &route.referral.aname)?,
            msize: self.state.service_msize,
            auth_config: self.state.authorities.session_auth_config(&authority)?,
            authorities: AuthorityBindings::new(),
        };
        let connect_timeout = route_connect_timeout(timeout, self.state.connect_timeout);
        let connected = DirectClient::connect_with_tracker_timeout(
            &config,
            self.state.tracker.clone(),
            connect_timeout,
        )?;
        self.state
            .connected_routes
            .lock()
            .map_err(|_| Error::new(libc::EIO, "namespace connection lock poisoned"))?
            .push(connected.clone());
        *client = Some(connected.clone());
        Ok(connected)
    }
}

impl Route {
    fn has_client(&self) -> bool {
        self.client
            .lock()
            .map(|client| client.is_some())
            .unwrap_or(false)
    }

    fn is_valid(&self) -> bool {
        self.received_at.elapsed() < Duration::from_millis(self.referral.valid_for_ms)
    }
}

fn build_routes(
    referrals: Vec<NamespaceReferral>,
    previous: &[Arc<Route>],
) -> Result<Vec<Arc<Route>>> {
    let received_at = Instant::now();
    let mut routes = Vec::with_capacity(referrals.len());
    for referral in referrals {
        referral.validate().map_err(protocol_error)?;
        if routes
            .iter()
            .any(|route: &Arc<Route>| route.referral.mount_path == referral.mount_path)
        {
            return Err(Error::new(
                libc::EPROTO,
                format!(
                    "duplicate namespace referral mount path {}",
                    String::from_utf8_lossy(&referral.mount_path)
                ),
            ));
        }
        let retained = previous
            .iter()
            .find(|route| same_route_identity(&route.referral, &referral))
            .and_then(|route| route.client.lock().ok().and_then(|client| client.clone()));
        routes.push(Arc::new(Route {
            referral,
            received_at,
            client: Mutex::new(retained),
        }));
    }
    routes.sort_by(|left, right| {
        right
            .referral
            .mount_path
            .len()
            .cmp(&left.referral.mount_path.len())
            .then_with(|| left.referral.mount_path.cmp(&right.referral.mount_path))
    });
    Ok(routes)
}

fn same_route_identity(left: &NamespaceReferral, right: &NamespaceReferral) -> bool {
    left.mount_path == right.mount_path
        && left.endpoint == right.endpoint
        && left.uname == right.uname
        && left.aname == right.aname
        && left.exported_root == right.exported_root
        && left.authority_boundary == right.authority_boundary
        && left.generation == right.generation
}

fn walk_remote(target: &RoutedTarget) -> Result<Fid> {
    if target.remote_path.is_empty() {
        target.client.clone_fid(target.client.root_fid())
    } else {
        target
            .client
            .walk(target.client.root_fid(), &target.remote_path)
    }
}

fn walk_remote_timeout(target: &RoutedTarget, timeout: Duration) -> Result<Fid> {
    if target.remote_path.is_empty() {
        target
            .client
            .clone_fid_timeout(target.client.root_fid(), timeout)
    } else {
        target
            .client
            .walk_timeout(target.client.root_fid(), &target.remote_path, timeout)
    }
}

fn apply_walk(start: &[Vec<u8>], names: &[Vec<u8>]) -> Result<Vec<Vec<u8>>> {
    let mut path = start.to_vec();
    for name in names {
        validate_path_element(name)?;
        match name.as_slice() {
            b"." => {}
            b".." => {
                path.pop();
            }
            _ => path.push(name.clone()),
        }
    }
    Ok(path)
}

pub(crate) fn parse_namespace_path(path: &[u8]) -> Result<Vec<Vec<u8>>> {
    if path == b"/" {
        return Ok(Vec::new());
    }
    let path = path.strip_prefix(b"/").unwrap_or(path);
    if path.is_empty() || path.ends_with(b"/") {
        return Err(Error::new(libc::EINVAL, "invalid namespace path"));
    }
    path.split(|byte| *byte == b'/')
        .map(|name| {
            validate_path_element(name)?;
            if matches!(name, b"." | b"..") {
                return Err(Error::new(libc::EINVAL, "namespace path must be canonical"));
            }
            Ok(name.to_vec())
        })
        .collect()
}

fn validate_path_element(name: &[u8]) -> Result<()> {
    if name.is_empty()
        || name.contains(&b'/')
        || name.contains(&0)
        || name.len() > usize::from(u16::MAX)
    {
        return Err(Error::new(libc::EINVAL, "invalid 9P path element"));
    }
    Ok(())
}

fn render_namespace_path(path: &[Vec<u8>]) -> Vec<u8> {
    if path.is_empty() {
        return b"/".to_vec();
    }
    let mut rendered = Vec::with_capacity(path.iter().map(Vec::len).sum::<usize>() + path.len());
    for name in path {
        rendered.push(b'/');
        rendered.extend_from_slice(name);
    }
    rendered
}

fn mounted_suffix<'a>(path: &'a [u8], mount_path: &[u8]) -> Option<&'a [u8]> {
    if path == mount_path {
        return Some(&[]);
    }
    path.strip_prefix(mount_path)
        .filter(|suffix| suffix.starts_with(b"/"))
}

fn text_field(field: &str, value: &[u8]) -> Result<String> {
    str::from_utf8(value).map(str::to_owned).map_err(|_| {
        Error::new(
            libc::EPROTO,
            format!("namespace referral {field} is not UTF-8"),
        )
    })
}

fn next_fid(fid: Fid) -> Fid {
    fid.checked_add(1).unwrap_or(FIRST_DYNAMIC_FID)
}

fn bounded_referral_timeout(timeout: Duration) -> Duration {
    if timeout.is_zero() {
        DEFAULT_REFERRAL_TIMEOUT
    } else {
        timeout.min(DEFAULT_REFERRAL_TIMEOUT)
    }
}

fn route_connect_timeout(operation: Duration, configured: Duration) -> Duration {
    match (operation.is_zero(), configured.is_zero()) {
        (false, false) => operation.min(configured),
        (false, true) => operation,
        (true, false) => configured,
        (true, true) => Duration::ZERO,
    }
}

fn protocol_error(error: r9p::Error) -> Error {
    Error::new(libc::EPROTO, error.display_lossy().to_string())
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn walk_normalization_cannot_escape_the_root() {
        assert_eq!(
            apply_walk(&[b"sources".to_vec()], &[b"..".to_vec(), b"..".to_vec()]).expect("walk"),
            Vec::<Vec<u8>>::new()
        );
    }

    #[test]
    fn longest_mount_prefix_wins_without_prefix_collisions() {
        assert_eq!(mounted_suffix(b"/sources/x", b"/sources/x"), Some(&[][..]));
        assert_eq!(
            mounted_suffix(b"/sources/x/search", b"/sources/x"),
            Some(&b"/search"[..])
        );
        assert_eq!(mounted_suffix(b"/sources/xyz", b"/sources/x"), None);
    }

    #[test]
    fn namespace_paths_round_trip_as_bytes() {
        let path = vec![b"sources".to_vec(), b"reddit".to_vec()];
        assert_eq!(
            parse_namespace_path(&render_namespace_path(&path)).expect("parse"),
            path
        );
    }
}
