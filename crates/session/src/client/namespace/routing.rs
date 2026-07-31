use super::*;

impl Route {
    pub(super) fn has_client(&self) -> bool {
        self.client
            .lock()
            .map(|client| client.is_some())
            .unwrap_or(false)
    }

    pub(super) fn is_valid(&self) -> bool {
        self.received_at.elapsed() < Duration::from_millis(self.referral.valid_for_ms)
    }
}

pub(super) fn build_routes(
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

pub(super) fn route_transport_failed(error: &Error) -> bool {
    error.is_definitive_transport_failure()
}

pub(super) fn walk_remote(target: &RoutedTarget) -> Result<Fid> {
    if target.remote_path.is_empty() {
        target.client.clone_fid(target.client.root_fid())
    } else {
        target
            .client
            .walk(target.client.root_fid(), &target.remote_path)
    }
}

pub(super) fn walk_remote_timeout(target: &RoutedTarget, timeout: Duration) -> Result<Fid> {
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

pub(super) fn apply_walk(start: &[Vec<u8>], names: &[Vec<u8>]) -> Result<Vec<Vec<u8>>> {
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

pub(super) fn render_namespace_path(path: &[Vec<u8>]) -> Vec<u8> {
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

pub(super) fn mounted_suffix<'a>(path: &'a [u8], mount_path: &[u8]) -> Option<&'a [u8]> {
    if path == mount_path {
        return Some(&[]);
    }
    path.strip_prefix(mount_path)
        .filter(|suffix| suffix.starts_with(b"/"))
}

pub(super) fn text_field(field: &str, value: &[u8]) -> Result<String> {
    str::from_utf8(value).map(str::to_owned).map_err(|_| {
        Error::new(
            libc::EPROTO,
            format!("namespace referral {field} is not UTF-8"),
        )
    })
}

pub(super) fn next_fid(fid: Fid) -> Fid {
    fid.checked_add(1).unwrap_or(FIRST_DYNAMIC_FID)
}

pub(super) fn bounded_referral_timeout(timeout: Duration) -> Duration {
    if timeout.is_zero() {
        DEFAULT_REFERRAL_TIMEOUT
    } else {
        timeout.min(DEFAULT_REFERRAL_TIMEOUT)
    }
}

pub(super) fn route_connect_timeout(operation: Duration, configured: Duration) -> Duration {
    match (operation.is_zero(), configured.is_zero()) {
        (false, false) => operation.min(configured),
        (false, true) => operation,
        (true, false) => configured,
        (true, true) => Duration::ZERO,
    }
}

pub(super) fn protocol_error(error: r9p::Error) -> Error {
    Error::new(libc::EPROTO, error.display_lossy().to_string())
}
