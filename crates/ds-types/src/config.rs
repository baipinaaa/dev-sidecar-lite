//! `running.json` 的结构映射。
//!
//! 字段命名与 JS 侧一一对应（`camelCase`），值类型做了宽松处理：
//! 无法识别的键统一收进各结构体的 `extra`，保证不丢数据。

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::flex::{de_string_or_vec_map, FlexBool};

/// 通用 JSON 映射（保留原始顺序）。
pub type JsonMap = IndexMap<String, Value>;

/// 单个域名的 intercept 表：`pattern -> options`。
///
/// 值为 `null` 表示显式排除（不拦截）。
pub type InterceptTable = IndexMap<String, Option<InterceptOptions>>;

fn default_true() -> FlexBool {
    FlexBool(true)
}

// ---------------------------------------------------------------------------
// 顶层
// ---------------------------------------------------------------------------

/// `running.json` 全量结构。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RuntimeConfig {
    pub setting: Setting,
    pub cloudflare_route: CloudflareRoute,

    /// 域名（可含正则/通配）→ 路径正则 → 拦截选项
    pub intercepts: IndexMap<String, InterceptTable>,

    pub dns: DnsConfig,

    /// 域名 → { ip: 是否启用 }
    pub pre_set_ip_list: IndexMap<String, IndexMap<String, FlexBool>>,

    /// 域名 → 直连
    pub white_list: IndexMap<String, FlexBool>,

    /// 域名 → 上游域名列表（S302 的 `dynamic a` 语义）
    #[serde(default, deserialize_with = "de_string_or_vec_map")]
    pub dynamic_upstreams: IndexMap<String, Vec<String>>,

    pub enabled: bool,
    pub host: String,
    pub port: u16,
    pub fake_server_max_length: usize,
    pub tls_fragment: TlsFragment,
    pub compatible: Compatible,
    pub intercept: InterceptGlobal,
    pub app: JsonMap,
    pub plugin: JsonMap,
    pub proxy: ProxyConfig,

    #[serde(flatten)]
    pub extra: JsonMap,
}

impl RuntimeConfig {
    /// 解析 JSON 文本。
    pub fn from_json_str(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }

    /// 监听地址，用于日志与自检。
    pub fn listen_addr(&self) -> String {
        let host = if self.host.is_empty() {
            "127.0.0.1"
        } else {
            self.host.as_str()
        };
        format!("{host}:{}", self.port)
    }

    /// 该域名是否在 `whiteList` 中（直连，不拦截）。
    pub fn is_white_listed(&self, hostname: &str) -> bool {
        self.white_list.iter().any(|(pattern, enabled)| {
            enabled.get() && crate::matcher::glob_match(pattern, hostname)
        })
    }
}

// ---------------------------------------------------------------------------
// setting
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Setting {
    pub script: ScriptSetting,
    pub tls_version_mapping: IndexMap<String, String>,

    /// 键名是 SCREAMING_SNAKE，不能走 camelCase 规则
    #[serde(rename = "NODE_TLS_REJECT_UNAUTHORIZED")]
    pub node_tls_reject_unauthorized: FlexBool,

    #[serde(default = "default_true")]
    pub verify_ssl: FlexBool,
    pub allow_tls12: FlexBool,
    pub user_base_path: String,
    pub root_ca_file: RootCaFile,
    pub default_timeout: u64,
    pub default_keep_alive_timeout: u64,
    pub timeout_mapping: IndexMap<String, TimeoutEntry>,
    pub low_speed_delay: u64,

    #[serde(flatten)]
    pub extra: JsonMap,
}

impl Default for Setting {
    fn default() -> Self {
        Self {
            script: ScriptSetting::default(),
            tls_version_mapping: IndexMap::new(),
            node_tls_reject_unauthorized: FlexBool(false),
            verify_ssl: FlexBool(true),
            allow_tls12: FlexBool(false),
            user_base_path: String::new(),
            root_ca_file: RootCaFile::default(),
            default_timeout: 20_000,
            default_keep_alive_timeout: 30_000,
            timeout_mapping: IndexMap::new(),
            low_speed_delay: 200,
            extra: JsonMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ScriptSetting {
    pub enabled: bool,
    pub default_dir: String,
    #[serde(flatten)]
    pub extra: JsonMap,
}

impl Default for ScriptSetting {
    fn default() -> Self {
        Self {
            enabled: false,
            default_dir: "./extra/scripts/".to_string(),
            extra: JsonMap::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RootCaFile {
    pub cert_path: String,
    pub key_path: String,
    #[serde(flatten)]
    pub extra: JsonMap,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TimeoutEntry {
    pub timeout: u64,
    pub keep_alive_timeout: u64,
    #[serde(flatten)]
    pub extra: JsonMap,
}

// ---------------------------------------------------------------------------
// cloudflareRoute / tlsFragment / intercept / compatible
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CloudflareRoute {
    /// `blacklist` | `whitelist` | `off`
    pub mode: String,
    pub domains: JsonMap,
    pub enabled: bool,
    pub preferred_endpoint: String,
    #[serde(flatten)]
    pub extra: JsonMap,
}

impl Default for CloudflareRoute {
    fn default() -> Self {
        Self {
            mode: "blacklist".to_string(),
            domains: JsonMap::new(),
            enabled: false,
            preferred_endpoint: String::new(),
            extra: JsonMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TlsFragment {
    pub enabled: bool,
    pub size: usize,
    pub max_bytes: usize,
    pub domains: Vec<String>,
    #[serde(flatten)]
    pub extra: JsonMap,
}

impl Default for TlsFragment {
    fn default() -> Self {
        Self {
            enabled: true,
            size: 32,
            max_bytes: 2048,
            domains: Vec::new(),
            extra: JsonMap::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Compatible {
    pub connect: JsonMap,
    pub request: JsonMap,
    #[serde(flatten)]
    pub extra: JsonMap,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct InterceptGlobal {
    pub enabled: bool,
    #[serde(flatten)]
    pub extra: JsonMap,
}

impl Default for InterceptGlobal {
    fn default() -> Self {
        Self {
            enabled: true,
            extra: JsonMap::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// proxy
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ProxyConfig {
    pub enabled: bool,
    /// PAC 自动分流开关（本定制版新增）
    pub pac_enabled: bool,
    /// 直连域名列表（值为 `true` 才生效）
    pub exclude_ip_list: IndexMap<String, FlexBool>,
    pub exclude_domestic_domain_allow_list: FlexBool,
    pub remote_domestic_domain_allow_list_file_url: String,
    pub proxy_http: FlexBool,
    pub set_env: FlexBool,
    pub set_ca_bundle: FlexBool,
    pub name: String,
    #[serde(rename = "use")]
    pub use_: String,
    pub other: Vec<Value>,
    pub auto_update_domestic_domain_allow_list: FlexBool,
    pub domestic_domain_allow_list_file_path: String,

    #[serde(flatten)]
    pub extra: JsonMap,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            pac_enabled: false,
            exclude_ip_list: IndexMap::new(),
            exclude_domestic_domain_allow_list: FlexBool(true),
            remote_domestic_domain_allow_list_file_url: String::new(),
            proxy_http: FlexBool(true),
            set_env: FlexBool(true),
            set_ca_bundle: FlexBool(true),
            name: String::new(),
            use_: "local".to_string(),
            other: Vec::new(),
            auto_update_domestic_domain_allow_list: FlexBool(true),
            domestic_domain_allow_list_file_path: String::new(),
            extra: JsonMap::new(),
        }
    }
}

impl ProxyConfig {
    /// `excludeIpList` 中真正启用（值为 `true`）的域名。
    pub fn active_exclude_domains(&self) -> Vec<&str> {
        self.exclude_ip_list
            .iter()
            .filter(|(_, v)| v.get())
            .map(|(k, _)| k.as_str())
            .collect()
    }
}

// ---------------------------------------------------------------------------
// intercept 选项
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct InterceptOptions {
    pub sni: Option<String>,
    pub verify_host: Option<String>,
    pub cache_days: Option<u32>,
    pub desc: Option<String>,
    pub status: Option<u16>,
    /// 可能是字符串（代理名）或对象
    pub proxy: Option<Value>,
    pub response_replace: Option<ReplaceRules>,
    pub request_replace: Option<ReplaceRules>,
    pub script: Option<Value>,
    pub tampermonkey_script: Option<Value>,
    pub success: Option<Value>,
    pub abort: Option<Value>,
    pub backup: Option<Value>,
    pub redirect: Option<Value>,
    pub options: Option<Value>,

    #[serde(flatten)]
    pub extra: JsonMap,
}

impl InterceptOptions {
    /// 是否要求移除 `alt-svc`（阻断浏览器走 HTTP/3）。
    pub fn removes_alt_svc(&self) -> bool {
        self.response_replace
            .as_ref()
            .and_then(|r| r.headers.as_ref())
            .and_then(|h| h.get("alt-svc"))
            .and_then(|v| v.as_str())
            .map(|v| v.eq_ignore_ascii_case("[remove]"))
            .unwrap_or(false)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ReplaceRules {
    /// 头名 → 替换值（`[remove]` 表示删除）
    pub headers: Option<IndexMap<String, Value>>,
    pub body: Option<Value>,
    pub do_download: Option<FlexBool>,
    pub desc: Option<String>,

    #[serde(flatten)]
    pub extra: JsonMap,
}

impl ReplaceRules {
    /// 删除式替换（值为 `[remove]`）。
    pub fn removed_headers(&self) -> Vec<&str> {
        let Some(headers) = self.headers.as_ref() else {
            return Vec::new();
        };
        headers
            .iter()
            .filter(|(_, v)| {
                v.as_str()
                    .map(|s| s.eq_ignore_ascii_case("[remove]"))
                    .unwrap_or(false)
            })
            .map(|(k, _)| k.as_str())
            .collect()
    }
}

// ---------------------------------------------------------------------------
// dns
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DnsConfig {
    /// 域名 → "4" | "6"
    pub family_mapping: IndexMap<String, String>,
    pub providers: IndexMap<String, DnsProvider>,
    /// 域名 → provider 名
    pub mapping: IndexMap<String, Option<String>>,
    pub ech: EchConfig,
    pub nat64: Nat64Config,
    pub speed_test: SpeedTestConfig,

    #[serde(flatten)]
    pub extra: JsonMap,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DnsProvider {
    /// `udp://` / `tls://` / `https://` / `h3://` 或裸 IP
    pub server: String,
    pub sni: Option<String>,
    pub for_sni: FlexBool,

    #[serde(flatten)]
    pub extra: JsonMap,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct EchConfig {
    pub enabled: bool,
    #[serde(rename = "use")]
    pub use_ech: bool,
    /// ECH 查询首选 provider 名
    pub dns: Option<String>,
    pub domains: Vec<String>,
    pub pre_set_ip_domains: Vec<String>,
    pub try_all_providers: bool,
    /// 公开名（Cloudflare 系为 `cloudflare-ech.com`）
    pub public_name: Option<String>,
    pub cache_size: usize,
    pub empty_ttl: u64,
    pub min_ttl: u64,
    pub max_ttl: u64,
    pub parallel_delay: u64,

    #[serde(flatten)]
    pub extra: JsonMap,
}

impl Default for EchConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            use_ech: true,
            dns: None,
            domains: Vec::new(),
            pre_set_ip_domains: Vec::new(),
            // 默认关闭跨 provider 竞速（浏览器语义：A 与 ECH 同源）
            try_all_providers: false,
            public_name: Some("cloudflare-ech.com".to_string()),
            cache_size: 512,
            empty_ttl: 600_000,
            min_ttl: 60_000,
            max_ttl: 3_600_000,
            parallel_delay: 200,
            extra: JsonMap::new(),
        }
    }
}

impl EchConfig {
    pub fn is_active(&self) -> bool {
        self.enabled && self.use_ech
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Nat64Config {
    pub enabled: bool,
    pub dns: Option<String>,
    pub domains: Vec<String>,
    pub prefix: String,

    #[serde(flatten)]
    pub extra: JsonMap,
}

impl Default for Nat64Config {
    fn default() -> Self {
        Self {
            enabled: false,
            dns: None,
            domains: Vec::new(),
            prefix: String::new(),
            extra: JsonMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SpeedTestConfig {
    pub enabled: bool,
    pub interval: u64,
    pub hostname_list: Vec<String>,
    pub dns_providers: Vec<String>,

    #[serde(flatten)]
    pub extra: JsonMap,
}

impl Default for SpeedTestConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval: 60_000,
            hostname_list: Vec::new(),
            dns_providers: Vec::new(),
            extra: JsonMap::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// 便捷派生
// ---------------------------------------------------------------------------

impl RuntimeConfig {
    /// `ech.domains` 中真正启用的规则（去空项）。
    pub fn ech_domains(&self) -> Vec<&str> {
        self.dns
            .ech
            .domains
            .iter()
            .map(|s| s.as_str())
            .filter(|s| !s.is_empty())
            .collect()
    }

    /// `preSetIpList` 中真正启用的 IP（值为 `true`）。
    pub fn active_preset_ips(&self, hostname: &str) -> Vec<&str> {
        let Some(map) = self
            .pre_set_ip_list
            .iter()
            .find(|(pattern, _)| crate::matcher::glob_match(pattern, hostname))
            .map(|(_, v)| v)
        else {
            return Vec::new();
        };
        map.iter()
            .filter(|(_, enabled)| enabled.get())
            .map(|(ip, _)| ip.as_str())
            .collect()
    }

    /// 该域名对应的动态上游（第一个匹配的规则）。
    pub fn dynamic_upstreams_for(&self, hostname: &str) -> Vec<&str> {
        self.dynamic_upstreams
            .iter()
            .find(|(pattern, _)| crate::matcher::glob_match(pattern, hostname))
            .map(|(_, list)| list.iter().map(|s| s.as_str()).collect())
            .unwrap_or_default()
    }
}

/// `fakeServerMaxLength` 的兜底默认值（与 JS 侧一致）。
pub const DEFAULT_FAKE_SERVER_MAX_LENGTH: usize = 5 * 1024 * 1024;

/// 请求体缓存上限（与 JS 侧 `bufferRequestBody` 默认值一致）。
pub const DEFAULT_MAX_BODY: usize = 5 * 1024 * 1024;
