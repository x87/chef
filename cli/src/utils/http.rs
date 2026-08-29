use anyhow::bail;
use std::io::Read as _;

pub const USER_AGENT: &str = concat!("chef/", env!("CARGO_PKG_VERSION"));

/// Shared HTTP agent for every network call
pub fn http_agent(timeout: std::time::Duration) -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(15))
        .timeout(timeout)
        .user_agent(USER_AGENT)
        .redirects(5)
        .build()
}

/// GET `url`, enforcing the caller's `max_bytes` cap.
pub fn fetch_url(
    url: &str,
    max_bytes: u64,
    timeout: std::time::Duration,
) -> anyhow::Result<Vec<u8>> {
    let resp = http_agent(timeout)
        .get(url)
        .call()
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut reader = resp.into_reader().take(max_bytes.saturating_add(1));
    let mut buf = Vec::new();
    std::io::Read::read_to_end(&mut reader, &mut buf)?;
    if buf.len() as u64 > max_bytes {
        bail!("response exceeds {max_bytes} byte limit");
    }
    Ok(buf)
}
