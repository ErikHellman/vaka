use std::env;

#[derive(Clone, Debug)]
pub struct Config {
    pub listen: String,
    pub public_url: String,
    pub db_url: String,
    pub clickhouse_url: String,
    pub clickhouse_user: String,
    pub clickhouse_password: String,
    pub clickhouse_database: String,
    pub admin_email: Option<String>,
    pub admin_password: Option<String>,
    pub data_dir: String,
    pub geoip_db: Option<String>,
    pub resend_api_key: Option<String>,
    pub resend_api_url: String,
    /// Optional: write digest narratives with any OpenAI-compatible chat completions API.
    /// Enabled when both base URL and model are set; the key is optional (local servers).
    pub llm_base_url: Option<String>,
    pub llm_model: Option<String>,
    pub llm_api_key: Option<String>,
    /// Allow email + password login in the admin UI. Email codes are offered whenever email is configured.
    pub password_login: bool,
    pub email_from: String,
    pub default_retention_days: i64,
    pub secure_cookies: bool,
    /// GitHub REST API base for the github_dispatch / github_issue alert channels.
    pub github_api_url: String,
    /// User feedback accepted per client IP and per session per hour (0 = unlimited).
    pub feedback_ip_limit: u32,
    pub feedback_session_limit: u32,
    /// Peers whose `X-Forwarded-For` / `X-Real-IP` headers are believed (`VAKA_TRUSTED_PROXIES`).
    pub trusted_proxies: Vec<Cidr>,
}

/// An IP network such as `10.0.0.0/8` or `203.0.113.7` (a single address).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cidr {
    pub addr: std::net::IpAddr,
    pub prefix: u8,
}

impl Cidr {
    pub fn parse(s: &str) -> Option<Cidr> {
        let s = s.trim();
        let (a, p) = match s.split_once('/') {
            Some((a, p)) => (a, Some(p.parse::<u8>().ok()?)),
            None => (s, None),
        };
        let addr: std::net::IpAddr = a.parse().ok()?;
        let max = if addr.is_ipv4() { 32 } else { 128 };
        let prefix = p.unwrap_or(max);
        (prefix <= max).then_some(Cidr { addr, prefix })
    }

    pub fn contains(&self, ip: std::net::IpAddr) -> bool {
        use std::net::IpAddr;
        let ip = match ip {
            IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
            v4 => v4,
        };
        match (self.addr, ip) {
            (IpAddr::V4(n), IpAddr::V4(a)) => {
                let mask = if self.prefix == 0 { 0 } else { u32::MAX << (32 - self.prefix as u32) };
                u32::from(n) & mask == u32::from(a) & mask
            }
            (IpAddr::V6(n), IpAddr::V6(a)) => {
                let mask = if self.prefix == 0 { 0 } else { u128::MAX << (128 - self.prefix as u32) };
                u128::from(n) & mask == u128::from(a) & mask
            }
            _ => false,
        }
    }
}

/// Loopback, private (RFC 1918), CGNAT, link-local and unique-local ranges: where reverse proxies,
/// Docker networks and platform load balancers usually connect from.
pub const DEFAULT_TRUSTED_PROXIES: &str = "127.0.0.0/8,::1/128,10.0.0.0/8,172.16.0.0/12,192.168.0.0/16,100.64.0.0/10,169.254.0.0/16,fc00::/7,fe80::/10";

/// Parse a comma-separated list; `none` (or an empty value) trusts nobody.
pub fn parse_trusted_proxies(s: &str) -> Vec<Cidr> {
    if s.trim().eq_ignore_ascii_case("none") {
        return Vec::new();
    }
    s.split(',').filter(|p| !p.trim().is_empty()).filter_map(|p| {
        let c = Cidr::parse(p);
        if c.is_none() {
            tracing::warn!("VAKA_TRUSTED_PROXIES: ignoring {p:?} (not an IP address or CIDR)");
        }
        c
    }).collect()
}

fn var(name: &str) -> Option<String> {
    env::var(name).ok().filter(|v| !v.trim().is_empty())
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let data_dir = var("VAKA_DATA_DIR").unwrap_or_else(|| "./data".into());
        let public_url = var("VAKA_PUBLIC_URL").unwrap_or_else(|| "http://localhost:8080".into());
        Ok(Self {
            listen: var("VAKA_LISTEN").unwrap_or_else(|| "0.0.0.0:8080".into()),
            secure_cookies: public_url.starts_with("https://"),
            public_url: public_url.trim_end_matches('/').to_string(),
            db_url: var("VAKA_DB_URL").unwrap_or_else(|| format!("sqlite://{data_dir}/vaka.db?mode=rwc")),
            clickhouse_url: var("VAKA_CLICKHOUSE_URL").unwrap_or_else(|| "http://localhost:8123".into()),
            clickhouse_user: var("VAKA_CLICKHOUSE_USER").unwrap_or_else(|| "default".into()),
            clickhouse_password: var("VAKA_CLICKHOUSE_PASSWORD").unwrap_or_default(),
            clickhouse_database: var("VAKA_CLICKHOUSE_DATABASE").unwrap_or_else(|| "vaka".into()),
            admin_email: var("VAKA_ADMIN_EMAIL"),
            admin_password: var("VAKA_ADMIN_PASSWORD"),
            geoip_db: var("VAKA_GEOIP_DB"),
            resend_api_key: var("VAKA_RESEND_API_KEY"),
            resend_api_url: var("VAKA_RESEND_API_URL").unwrap_or_else(|| "https://api.resend.com".into()).trim_end_matches('/').to_string(),
            llm_base_url: var("VAKA_LLM_BASE_URL").map(|u| u.trim_end_matches('/').to_string()),
            llm_model: var("VAKA_LLM_MODEL"),
            llm_api_key: var("VAKA_LLM_API_KEY"),
            password_login: !matches!(var("VAKA_PASSWORD_LOGIN").as_deref().map(str::to_lowercase).as_deref(), Some("false" | "0" | "no" | "off")),
            email_from: var("VAKA_EMAIL_FROM").unwrap_or_else(|| "Vaka <vaka@localhost>".into()),
            default_retention_days: var("VAKA_DEFAULT_RETENTION_DAYS")
                .and_then(|v| v.parse().ok())
                .unwrap_or(90),
            github_api_url: var("VAKA_GITHUB_API_URL").unwrap_or_else(|| "https://api.github.com".into()).trim_end_matches('/').to_string(),
            feedback_ip_limit: var("VAKA_FEEDBACK_PER_IP_HOUR").and_then(|v| v.parse().ok()).unwrap_or(10),
            feedback_session_limit: var("VAKA_FEEDBACK_PER_SESSION_HOUR").and_then(|v| v.parse().ok()).unwrap_or(5),
            trusted_proxies: parse_trusted_proxies(&std::env::var("VAKA_TRUSTED_PROXIES").unwrap_or_else(|_| DEFAULT_TRUSTED_PROXIES.into())),
            data_dir,
        })
    }

    /// Whether the admin UI can send one-time login codes by email.
    pub fn email_login(&self) -> bool {
        self.resend_api_key.is_some()
    }

    /// Password login stays on when email is not configured, so nobody gets locked out.
    pub fn password_login_enabled(&self) -> bool {
        self.password_login || !self.email_login()
    }

    pub fn artifact_dir(&self) -> String {
        format!("{}/artifacts", self.data_dir)
    }
}
