mod direct;
mod paths;

#[cfg(test)]
mod tests;

pub(crate) use namespace::parse_namespace_path;
pub use namespace::Client;

mod namespace;
