use super::direct::DirectClient;
use crate::{
    AuthorityBindings, ConnectionConfig, Error, RequestTracker, Result, WriteThenReadError,
};
use r9p::{
    fid::Fid,
    multiplex::{DelimitedRead, PendingRead as MultiplexedPendingRead},
    qid::Qid,
    referral::NamespaceReferral,
    stat::Stat,
    Tag, Variant, NOFID,
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

mod operations;
mod referral_directory;
mod routing;

pub(crate) use routing::parse_namespace_path;
use routing::{
    apply_walk, bounded_referral_timeout, build_routes, mounted_suffix, next_fid, protocol_error,
    render_namespace_path, route_connect_timeout, route_transport_failed, text_field,
    validate_path_element, walk_remote, walk_remote_timeout,
};

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
    namespace_path: Vec<Vec<u8>>,
    target: FidTarget,
}

#[derive(Clone)]
enum FidTarget {
    Remote(RemoteFid),
    ReferralDirectory,
}

#[derive(Clone)]
struct RemoteFid {
    client: DirectClient,
    fid: Fid,
    route_mount: Option<Vec<u8>>,
}

impl FidBinding {
    fn remote(&self) -> Result<RemoteFid> {
        match &self.target {
            FidTarget::Remote(remote) => Ok(remote.clone()),
            FidTarget::ReferralDirectory => Err(referral_directory::directory_operation_error()),
        }
    }

    fn writable_remote(&self) -> Result<RemoteFid> {
        match &self.target {
            FidTarget::Remote(remote) => Ok(remote.clone()),
            FidTarget::ReferralDirectory => Err(referral_directory::read_only_error()),
        }
    }
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

fn remote_target(target: RoutedTarget, remote_fid: Fid) -> FidTarget {
    FidTarget::Remote(RemoteFid {
        client: target.client,
        fid: remote_fid,
        route_mount: target.route_mount,
    })
}

pub(crate) struct PendingRead {
    client: DirectClient,
    pending: MultiplexedPendingRead,
}

impl PendingRead {
    pub(crate) fn tag(&self) -> Tag {
        self.pending.tag()
    }

    pub(crate) fn wait(self) -> Result<Vec<u8>> {
        self.client.wait_read(self.pending)
    }

    pub(crate) fn wait_timeout(self, timeout: Duration) -> Result<Vec<u8>> {
        self.client.wait_read_timeout(self.pending, timeout)
    }
}

impl Client {
    pub fn same_session(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.state, &other.state)
    }

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
            namespace_path: Vec::new(),
            target: FidTarget::Remote(RemoteFid {
                client: root.clone(),
                fid: root.root_fid(),
                route_mount: None,
            }),
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
        let target = match &binding.target {
            FidTarget::Remote(remote) => FidTarget::Remote(RemoteFid {
                client: remote.client.clone(),
                fid: remote.client.clone_fid(remote.fid)?,
                route_mount: remote.route_mount.clone(),
            }),
            FidTarget::ReferralDirectory => FidTarget::ReferralDirectory,
        };
        self.allocate_binding(FidBinding { target, ..binding })
    }

    pub fn clone_fid_timeout(&self, fid: Fid, timeout: Duration) -> Result<Fid> {
        let binding = self.binding(fid)?;
        let target = match &binding.target {
            FidTarget::Remote(remote) => FidTarget::Remote(RemoteFid {
                client: remote.client.clone(),
                fid: remote.client.clone_fid_timeout(remote.fid, timeout)?,
                route_mount: remote.route_mount.clone(),
            }),
            FidTarget::ReferralDirectory => FidTarget::ReferralDirectory,
        };
        self.allocate_binding(FidBinding { target, ..binding })
    }

    pub fn walk_one_timeout(&self, fid: Fid, name: &[u8], timeout: Duration) -> Result<Fid> {
        self.walk_timeout(fid, &[name.to_vec()], timeout)
    }

    pub fn walk_timeout(&self, fid: Fid, names: &[Vec<u8>], timeout: Duration) -> Result<Fid> {
        let binding = self.binding(fid)?;
        let namespace_path = apply_walk(&binding.namespace_path, names)?;
        let target = self.walk_namespace_path_timeout(&namespace_path, timeout)?;
        self.allocate_binding(FidBinding {
            namespace_path,
            target,
        })
    }

    pub fn walk_path(&self, path: &str) -> Result<Fid> {
        let namespace_path = parse_namespace_path(path.as_bytes())?;
        let target = self.walk_namespace_path(&namespace_path)?;
        self.allocate_binding(FidBinding {
            namespace_path,
            target,
        })
    }

    pub fn walk_path_timeout(&self, path: &str, timeout: Duration) -> Result<Fid> {
        let namespace_path = parse_namespace_path(path.as_bytes())?;
        let target = self.walk_namespace_path_timeout(&namespace_path, timeout)?;
        self.allocate_binding(FidBinding {
            namespace_path,
            target,
        })
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

    fn walk_namespace_path(&self, namespace_path: &[Vec<u8>]) -> Result<FidTarget> {
        let target = self.routed_target(namespace_path, self.state.connect_timeout)?;
        match walk_remote(&target) {
            Ok(fid) => Ok(remote_target(target, fid)),
            Err(error) if self.is_referral_directory_miss(namespace_path, &target, &error)? => {
                Ok(FidTarget::ReferralDirectory)
            }
            Err(error) if self.should_refresh_routes_after(&target, &error) => {
                self.invalidate_failed_route(&target, &error)?;
                self.refresh_routes(self.state.connect_timeout)?;
                let target = self.routed_target(namespace_path, self.state.connect_timeout)?;
                match walk_remote(&target) {
                    Ok(fid) => Ok(remote_target(target, fid)),
                    Err(error)
                        if self.is_referral_directory_miss(namespace_path, &target, &error)? =>
                    {
                        Ok(FidTarget::ReferralDirectory)
                    }
                    Err(error) => Err(error),
                }
            }
            Err(error) => Err(error),
        }
    }

    fn walk_namespace_path_timeout(
        &self,
        namespace_path: &[Vec<u8>],
        timeout: Duration,
    ) -> Result<FidTarget> {
        let target = self.routed_target(namespace_path, timeout)?;
        match walk_remote_timeout(&target, timeout) {
            Ok(fid) => Ok(remote_target(target, fid)),
            Err(error) if self.is_referral_directory_miss(namespace_path, &target, &error)? => {
                Ok(FidTarget::ReferralDirectory)
            }
            Err(error) if self.should_refresh_routes_after(&target, &error) => {
                self.invalidate_failed_route(&target, &error)?;
                self.refresh_routes(timeout)?;
                let target = self.routed_target(namespace_path, timeout)?;
                match walk_remote_timeout(&target, timeout) {
                    Ok(fid) => Ok(remote_target(target, fid)),
                    Err(error)
                        if self.is_referral_directory_miss(namespace_path, &target, &error)? =>
                    {
                        Ok(FidTarget::ReferralDirectory)
                    }
                    Err(error) => Err(error),
                }
            }
            Err(error) => Err(error),
        }
    }

    fn is_referral_directory_miss(
        &self,
        namespace_path: &[Vec<u8>],
        target: &RoutedTarget,
        error: &Error,
    ) -> Result<bool> {
        Ok(error.errno == libc::ENOENT
            && target.route_mount.is_none()
            && !namespace_path.is_empty()
            && referral_directory::is_ancestor(
                namespace_path,
                self.referral_mount_paths(|route| route.has_client() || route.is_valid())?,
            ))
    }

    fn should_refresh_routes_after(&self, target: &RoutedTarget, error: &Error) -> bool {
        if !self.variant().supports_referrals() {
            return false;
        }
        match error.errno {
            // A miss on the root attachment can mean a newly admitted
            // referral now owns the path. A miss inside an already selected
            // referral is ordinary service-local absence.
            libc::ENOENT => target.route_mount.is_none(),
            libc::ESTALE | libc::ENOTCONN | libc::ECONNABORTED | libc::ECONNRESET | libc::EPIPE => {
                true
            }
            _ => false,
        }
    }

    fn invalidate_failed_route(&self, target: &RoutedTarget, error: &Error) -> Result<()> {
        let Some(route_mount) = target.route_mount.as_deref() else {
            return Ok(());
        };
        if !route_transport_failed(error) {
            return Ok(());
        }
        self.invalidate_route_client(route_mount, &target.client)
    }

    pub(super) fn route_failure_context(
        &self,
        fid: Fid,
    ) -> Result<(DirectClient, Option<Vec<u8>>)> {
        let binding = self.binding(fid)?;
        match binding.target {
            FidTarget::Remote(remote) => Ok((remote.client, remote.route_mount)),
            FidTarget::ReferralDirectory => Ok((self.state.root.clone(), None)),
        }
    }

    pub(super) fn recover_read_only_route(
        &self,
        failed_fid: Fid,
        failed_client: &DirectClient,
        route_mount: Option<&[u8]>,
        error: &Error,
        timeout: Duration,
    ) -> Result<bool> {
        let Some(route_mount) = route_mount else {
            return Ok(false);
        };
        if !route_transport_failed(error) {
            return Ok(false);
        }
        self.discard_binding(failed_fid)?;
        self.invalidate_route_client(route_mount, failed_client)?;
        self.refresh_routes(timeout)?;
        Ok(true)
    }

    fn discard_binding(&self, fid: Fid) -> Result<()> {
        self.state
            .fids
            .lock()
            .map_err(|_| Error::new(libc::EIO, "namespace fid lock poisoned"))?
            .bindings
            .remove(&fid);
        Ok(())
    }

    fn invalidate_route_client(
        &self,
        route_mount: &[u8],
        failed_client: &DirectClient,
    ) -> Result<()> {
        let route = self
            .state
            .routes
            .lock()
            .map_err(|_| Error::new(libc::EIO, "namespace route lock poisoned"))?
            .iter()
            .find(|route| route.referral.mount_path == route_mount)
            .cloned();
        let Some(route) = route else {
            return Ok(());
        };
        let removed = {
            let mut current = route
                .client
                .lock()
                .map_err(|_| Error::new(libc::EIO, "namespace route client lock poisoned"))?;
            if current
                .as_ref()
                .is_some_and(|client| client.same_connection(failed_client))
            {
                current.take();
                true
            } else {
                false
            }
        };
        if removed {
            self.state
                .connected_routes
                .lock()
                .map_err(|_| Error::new(libc::EIO, "namespace connection lock poisoned"))?
                .retain(|client| !client.same_connection(failed_client));
        }
        Ok(())
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

    fn referral_mount_paths(&self, include: impl Fn(&Route) -> bool) -> Result<Vec<Vec<u8>>> {
        let routes = self
            .state
            .routes
            .lock()
            .map_err(|_| Error::new(libc::EIO, "namespace route lock poisoned"))?;
        Ok(routes
            .iter()
            .filter(|route| include(route))
            .map(|route| route.referral.mount_path.clone())
            .collect())
    }

    fn read_referral_directory(
        &self,
        namespace_path: &[Vec<u8>],
        offset: u64,
        count: u32,
    ) -> Result<Vec<u8>> {
        referral_directory::read(
            namespace_path,
            self.referral_mount_paths(|_| true)?,
            offset,
            count,
        )
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
        // The referral names the service; under XX the responder must prove that
        // name with its certificate. That is what makes a referral safe to take
        // from an addressing service: coordinator can point a session somewhere,
        // it cannot change who answers.
        let boundary = r9p::export_descriptor::AuthBoundary::parse(&authority).ok();
        let expected_responder = boundary
            .as_ref()
            .and_then(r9p::export_descriptor::AuthBoundary::p9any_domain);
        let config = ConnectionConfig {
            address: text_field("endpoint", &route.referral.endpoint)?,
            uname: text_field("uname", &route.referral.uname)?,
            aname: text_field("aname", &route.referral.aname)?,
            msize: self.state.service_msize,
            auth_config: self.state.authorities.session_auth_config(&authority)?,
            authorities: AuthorityBindings::new(),
        };
        let connect_timeout = route_connect_timeout(timeout, self.state.connect_timeout);
        let connected = DirectClient::connect_expecting(
            &config,
            self.state.tracker.clone(),
            connect_timeout,
            expected_responder,
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
