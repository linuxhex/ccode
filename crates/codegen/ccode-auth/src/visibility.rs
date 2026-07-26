/// Apply auth headers to outbound visibility requests.
/// Implemented by `ccode-shell::util::ccode_auth_credentials::CcodeAuthCredentials`
/// to keep credential construction owned by shell while letting data-collector
/// build the request without reaching back into shell types.
pub trait HttpAuth: Send + Sync {
    fn apply(&self, builder: reqwest::RequestBuilder, base_url: &str) -> reqwest::RequestBuilder;
}
