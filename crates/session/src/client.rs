mod direct;
mod paths;

#[cfg(test)]
mod tests;

pub use namespace::Client;
pub(crate) use namespace::parse_namespace_path;

mod namespace;
