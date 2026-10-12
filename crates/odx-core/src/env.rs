/// Read access to environment variables.
///
/// Library code asks for variables through this trait (for example token
/// lookups such as `GITHUB_TOKEN`), so tests never depend on the real
/// process environment.
pub trait Env: std::fmt::Debug + Send + Sync {
    /// The value of `key`, or `None` when it is unset or not valid Unicode.
    fn var(&self, key: &str) -> Option<String>;
}
