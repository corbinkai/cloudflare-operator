use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use rand::Rng;
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use tracing::{error, info, warn};

use crate::error::{Error, Result};

use super::types::{
    Account, CfListResponse, CfResponse, CfTunnel, CreateDnsRecordRequest, CreateTunnelRequest,
    DnsManagedRecordTxt, DnsRecord, TunnelConfig, TunnelConfigurationRequest, TunnelCredentials,
    TunnelIngressRule, UpdateDnsRecordRequest, Zone,
};

const DEFAULT_BASE_URL: &str = "https://api.cloudflare.com/client/v4";
const TXT_PREFIX: &str = "_managed.";

/// Authentication mode for the Cloudflare API.
enum AuthMode {
    Token(String),
    Key { api_key: String, email: String },
}

/// Cloudflare API client that wraps reqwest and handles authentication,
/// response envelope parsing, and cached/validated IDs.
pub struct CfClient {
    http: reqwest::Client,
    base_url: String,
    auth: AuthMode,

    pub account_id: String,
    pub tunnel_id: String,
    pub tunnel_name: String,
    pub zone_id: String,
    pub domain: String,
}

impl CfClient {
    /// Create a new client authenticated with an API token (Bearer auth).
    pub fn new(api_token: &str, base_url: Option<&str>) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: base_url.unwrap_or(DEFAULT_BASE_URL).trim_end_matches('/').to_string(),
            auth: AuthMode::Token(api_token.to_string()),
            account_id: String::new(),
            tunnel_id: String::new(),
            tunnel_name: String::new(),
            zone_id: String::new(),
            domain: String::new(),
        }
    }

    /// Create a new client authenticated with an API key + email.
    pub fn new_with_key(api_key: &str, email: &str, base_url: Option<&str>) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: base_url.unwrap_or(DEFAULT_BASE_URL).trim_end_matches('/').to_string(),
            auth: AuthMode::Key {
                api_key: api_key.to_string(),
                email: email.to_string(),
            },
            account_id: String::new(),
            tunnel_id: String::new(),
            tunnel_name: String::new(),
            zone_id: String::new(),
            domain: String::new(),
        }
    }

    /// Build auth headers based on the authentication mode.
    fn auth_headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        match &self.auth {
            AuthMode::Token(token) => {
                headers.insert(
                    AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {token}")).expect("valid header value"),
                );
            }
            AuthMode::Key { api_key, email } => {
                headers.insert(
                    "X-Auth-Key",
                    HeaderValue::from_str(api_key).expect("valid header value"),
                );
                headers.insert(
                    "X-Auth-Email",
                    HeaderValue::from_str(email).expect("valid header value"),
                );
            }
        }
        headers
    }

    /// Parse a single-result Cloudflare response envelope, returning the result or an error.
    fn parse_response<T>(resp: CfResponse<T>) -> Result<T> {
        if !resp.success {
            let msgs: Vec<String> =
                resp.errors.iter().map(|e| format!("[{}] {}", e.code, e.message)).collect();
            return Err(Error::Cloudflare(msgs.join("; ")));
        }
        resp.result
            .ok_or_else(|| Error::Cloudflare("response success but result is null".into()))
    }

    /// Parse a list-result Cloudflare response envelope.
    fn parse_list_response<T>(resp: CfListResponse<T>) -> Result<Vec<T>> {
        if !resp.success {
            let msgs: Vec<String> =
                resp.errors.iter().map(|e| format!("[{}] {}", e.code, e.message)).collect();
            return Err(Error::Cloudflare(msgs.join("; ")));
        }
        Ok(resp.result)
    }

    // ── Tunnel operations ───────────────────────────────────────────────

    /// Create a new Cloudflare Tunnel. Returns `(tunnel_id, credentials_json)`.
    pub async fn create_tunnel(
        &mut self,
        account_id: &str,
        name: &str,
    ) -> Result<(String, String)> {
        let mut secret_bytes = [0u8; 32];
        rand::rng().fill(&mut secret_bytes);
        let tunnel_secret = BASE64.encode(secret_bytes);

        let url = format!("{}/accounts/{}/cfd_tunnel", self.base_url, account_id);
        let body = CreateTunnelRequest {
            name: name.to_string(),
            tunnel_secret: tunnel_secret.clone(),
            config_src: "local".to_string(),
        };

        let resp = self
            .http
            .post(&url)
            .headers(self.auth_headers())
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::Cloudflare(format!("HTTP error creating tunnel: {e}")))?;

        let cf_resp: CfResponse<CfTunnel> = resp
            .json()
            .await
            .map_err(|e| Error::Cloudflare(format!("error parsing create tunnel response: {e}")))?;

        let tunnel = Self::parse_response(cf_resp)?;

        self.tunnel_id = tunnel.id.clone();
        self.tunnel_name = tunnel.name.clone();

        let creds = TunnelCredentials {
            account_tag: account_id.to_string(),
            tunnel_id: tunnel.id.clone(),
            tunnel_name: tunnel.name.clone(),
            tunnel_secret,
        };
        let creds_json = serde_json::to_string(&creds)?;

        info!(tunnel_id = %tunnel.id, tunnel_name = %tunnel.name, "created tunnel");
        Ok((tunnel.id, creds_json))
    }

    /// Delete the currently-validated tunnel. Cleans connections first, then deletes.
    pub async fn delete_tunnel(&self) -> Result<()> {
        // Clean connections
        let clean_url = format!(
            "{}/accounts/{}/cfd_tunnel/{}/connections",
            self.base_url, self.account_id, self.tunnel_id
        );
        let resp = self
            .http
            .delete(&clean_url)
            .headers(self.auth_headers())
            .send()
            .await
            .map_err(|e| {
                Error::Cloudflare(format!("HTTP error cleaning tunnel connections: {e}"))
            })?;

        let cf_resp: CfResponse<serde_json::Value> = resp
            .json()
            .await
            .map_err(|e| {
                Error::Cloudflare(format!("error parsing clean connections response: {e}"))
            })?;

        if !cf_resp.success {
            let msgs: Vec<String> =
                cf_resp.errors.iter().map(|e| format!("[{}] {}", e.code, e.message)).collect();
            error!(
                tunnel_id = %self.tunnel_id,
                errors = %msgs.join("; "),
                "error cleaning tunnel connections"
            );
            return Err(Error::Cloudflare(msgs.join("; ")));
        }

        // Delete tunnel
        let del_url = format!(
            "{}/accounts/{}/cfd_tunnel/{}",
            self.base_url, self.account_id, self.tunnel_id
        );
        let resp = self
            .http
            .delete(&del_url)
            .headers(self.auth_headers())
            .send()
            .await
            .map_err(|e| Error::Cloudflare(format!("HTTP error deleting tunnel: {e}")))?;

        let cf_resp: CfResponse<serde_json::Value> = resp
            .json()
            .await
            .map_err(|e| {
                Error::Cloudflare(format!("error parsing delete tunnel response: {e}"))
            })?;

        if !cf_resp.success {
            let msgs: Vec<String> =
                cf_resp.errors.iter().map(|e| format!("[{}] {}", e.code, e.message)).collect();
            error!(
                tunnel_id = %self.tunnel_id,
                errors = %msgs.join("; "),
                "error deleting tunnel"
            );
            return Err(Error::Cloudflare(msgs.join("; ")));
        }

        info!(tunnel_id = %self.tunnel_id, "deleted tunnel");
        Ok(())
    }

    /// Get a single tunnel by account and tunnel ID.
    pub async fn get_tunnel(&self, account_id: &str, tunnel_id: &str) -> Result<CfTunnel> {
        let url = format!(
            "{}/accounts/{}/cfd_tunnel/{}",
            self.base_url, account_id, tunnel_id
        );
        let resp = self
            .http
            .get(&url)
            .headers(self.auth_headers())
            .send()
            .await
            .map_err(|e| Error::Cloudflare(format!("HTTP error getting tunnel: {e}")))?;

        let cf_resp: CfResponse<CfTunnel> = resp
            .json()
            .await
            .map_err(|e| {
                Error::Cloudflare(format!("error parsing get tunnel response: {e}"))
            })?;

        Self::parse_response(cf_resp)
    }

    /// List tunnels in an account, filtered by name.
    pub async fn list_tunnels(&self, account_id: &str, name: &str) -> Result<Vec<CfTunnel>> {
        let url = format!("{}/accounts/{}/cfd_tunnel", self.base_url, account_id);
        let resp = self
            .http
            .get(&url)
            .headers(self.auth_headers())
            .query(&[("name", name)])
            .send()
            .await
            .map_err(|e| Error::Cloudflare(format!("HTTP error listing tunnels: {e}")))?;

        let cf_resp: CfListResponse<CfTunnel> = resp
            .json()
            .await
            .map_err(|e| {
                Error::Cloudflare(format!("error parsing list tunnels response: {e}"))
            })?;

        Self::parse_list_response(cf_resp)
    }

    // ── Validation ──────────────────────────────────────────────────────

    /// Validate and resolve account ID. Tries `account_id` first, then falls back to
    /// listing accounts by `account_name`. Returns the validated account ID.
    pub async fn validate_account(
        &mut self,
        account_id: &str,
        account_name: &str,
    ) -> Result<String> {
        if !self.account_id.is_empty() {
            return Ok(self.account_id.clone());
        }

        if account_id.is_empty() && account_name.is_empty() {
            return Err(Error::Cloudflare(
                "both account ID and name cannot be empty".into(),
            ));
        }

        // Try account ID first
        if !account_id.is_empty() {
            let url = format!("{}/accounts/{}", self.base_url, account_id);
            let resp = self
                .http
                .get(&url)
                .headers(self.auth_headers())
                .send()
                .await
                .map_err(|e| {
                    Error::Cloudflare(format!("HTTP error validating account: {e}"))
                })?;

            let cf_resp: CfResponse<Account> = resp
                .json()
                .await
                .map_err(|e| {
                    Error::Cloudflare(format!("error parsing account response: {e}"))
                })?;

            if cf_resp.success {
                if let Some(acct) = cf_resp.result {
                    if acct.id == account_id {
                        self.account_id = acct.id.clone();
                        return Ok(acct.id);
                    }
                }
            }
            info!("account ID validation failed, falling back to account name");
        }

        // Fall back to account name
        if account_name.is_empty() {
            return Err(Error::Cloudflare(
                "account ID invalid and no account name provided".into(),
            ));
        }

        let url = format!("{}/accounts", self.base_url);
        let resp = self
            .http
            .get(&url)
            .headers(self.auth_headers())
            .query(&[("name", account_name)])
            .send()
            .await
            .map_err(|e| Error::Cloudflare(format!("HTTP error listing accounts: {e}")))?;

        let cf_resp: CfListResponse<Account> = resp
            .json()
            .await
            .map_err(|e| {
                Error::Cloudflare(format!("error parsing accounts list response: {e}"))
            })?;

        let accounts = Self::parse_list_response(cf_resp)?;

        match accounts.len() {
            0 => Err(Error::Cloudflare(format!(
                "no account found for name '{account_name}'"
            ))),
            1 => {
                self.account_id = accounts[0].id.clone();
                Ok(accounts[0].id.clone())
            }
            _ => Err(Error::Cloudflare(format!(
                "multiple accounts found for name '{account_name}'"
            ))),
        }
    }

    /// Validate and resolve tunnel ID/name. Tries `tunnel_id` first, then falls back to
    /// listing tunnels by `tunnel_name`. Returns `(tunnel_id, tunnel_name)`.
    pub async fn validate_tunnel(
        &mut self,
        tunnel_id: &str,
        tunnel_name: &str,
    ) -> Result<(String, String)> {
        if !self.tunnel_id.is_empty() {
            return Ok((self.tunnel_id.clone(), self.tunnel_name.clone()));
        }

        if tunnel_id.is_empty() && tunnel_name.is_empty() {
            return Err(Error::Cloudflare(
                "both tunnel ID and name cannot be empty".into(),
            ));
        }

        // Try tunnel ID first
        if !tunnel_id.is_empty() {
            let acct_id = self.account_id.clone();
            match self.get_tunnel(&acct_id, tunnel_id).await {
                Ok(tunnel) => {
                    self.tunnel_id = tunnel.id.clone();
                    self.tunnel_name = tunnel.name.clone();
                    return Ok((tunnel.id, tunnel.name));
                }
                Err(e) => {
                    info!(
                        error = %e,
                        "tunnel ID validation failed, falling back to tunnel name"
                    );
                }
            }
        }

        // Fall back to tunnel name
        if tunnel_name.is_empty() {
            return Err(Error::Cloudflare(
                "tunnel ID invalid and no tunnel name provided".into(),
            ));
        }

        let acct_id = self.account_id.clone();
        let tunnels = self.list_tunnels(&acct_id, tunnel_name).await?;

        match tunnels.len() {
            0 => Err(Error::Cloudflare(format!(
                "no tunnel found for name '{tunnel_name}'"
            ))),
            1 => {
                self.tunnel_id = tunnels[0].id.clone();
                self.tunnel_name = tunnels[0].name.clone();
                Ok((tunnels[0].id.clone(), tunnels[0].name.clone()))
            }
            _ => Err(Error::Cloudflare(format!(
                "multiple tunnels found for name '{tunnel_name}'"
            ))),
        }
    }

    /// Validate and resolve zone ID from a domain name. Returns the zone ID.
    pub async fn validate_zone(&mut self, domain: &str) -> Result<String> {
        if !self.zone_id.is_empty() {
            return Ok(self.zone_id.clone());
        }

        if domain.is_empty() {
            return Err(Error::Cloudflare("domain cannot be empty".into()));
        }

        let url = format!("{}/zones", self.base_url);
        let resp = self
            .http
            .get(&url)
            .headers(self.auth_headers())
            .query(&[("name", domain)])
            .send()
            .await
            .map_err(|e| Error::Cloudflare(format!("HTTP error listing zones: {e}")))?;

        let cf_resp: CfListResponse<Zone> = resp
            .json()
            .await
            .map_err(|e| {
                Error::Cloudflare(format!("error parsing zones list response: {e}"))
            })?;

        let zones = Self::parse_list_response(cf_resp)?;

        match zones.len() {
            0 => Err(Error::Cloudflare(format!(
                "no zone found for domain '{domain}'"
            ))),
            1 => {
                self.zone_id = zones[0].id.clone();
                self.domain = domain.to_string();
                Ok(zones[0].id.clone())
            }
            _ => Err(Error::Cloudflare(format!(
                "multiple zones found for domain '{domain}'"
            ))),
        }
    }

    /// Validate account, tunnel, and zone in sequence.
    pub async fn validate_all(
        &mut self,
        account_id: &str,
        account_name: &str,
        tunnel_id: &str,
        tunnel_name: &str,
        domain: &str,
    ) -> Result<()> {
        info!("validating cloudflare resources");
        self.validate_account(account_id, account_name).await?;
        self.validate_tunnel(tunnel_id, tunnel_name).await?;
        self.validate_zone(domain).await?;
        info!(
            account_id = %self.account_id,
            tunnel_id = %self.tunnel_id,
            zone_id = %self.zone_id,
            "validation successful"
        );
        Ok(())
    }

    // ── DNS operations ──────────────────────────────────────────────────

    /// Get the ID of the CNAME record for the given FQDN.
    pub async fn get_dns_cname_id(&self, fqdn: &str) -> Result<String> {
        let records = self.list_dns_records("CNAME", fqdn).await?;

        match records.len() {
            0 => {
                info!(fqdn = %fqdn, "no CNAME records found");
                Err(Error::Cloudflare(format!(
                    "no CNAME records returned for '{fqdn}'"
                )))
            }
            1 => Ok(records[0].id.clone()),
            _ => {
                error!(fqdn = %fqdn, count = records.len(), "multiple CNAME records found");
                Err(Error::Cloudflare(format!(
                    "multiple CNAME records returned for '{fqdn}'"
                )))
            }
        }
    }

    /// Get the managed TXT record for the given FQDN.
    /// Returns `(txt_id, parsed_record, can_use_dns)`.
    ///
    /// `can_use_dns` is true if no TXT record exists (we can create one), or if the existing
    /// TXT record belongs to this tunnel. It is false if the record belongs to another tunnel
    /// or cannot be parsed.
    pub async fn get_managed_dns_txt(
        &self,
        fqdn: &str,
    ) -> Result<(String, DnsManagedRecordTxt, bool)> {
        let txt_name = format!("{TXT_PREFIX}{fqdn}");
        let records = self.list_dns_records("TXT", &txt_name).await?;

        let empty_txt = DnsManagedRecordTxt {
            dns_id: String::new(),
            tunnel_name: String::new(),
            tunnel_id: String::new(),
        };

        match records.len() {
            0 => {
                info!(fqdn = %fqdn, "no managed TXT records found");
                Ok((String::new(), empty_txt, true))
            }
            1 => {
                let record = &records[0];
                match serde_json::from_str::<DnsManagedRecordTxt>(&record.content) {
                    Ok(txt_data) => {
                        if txt_data.tunnel_id == self.tunnel_id {
                            Ok((record.id.clone(), txt_data, true))
                        } else {
                            warn!(
                                fqdn = %fqdn,
                                owner_tunnel = %txt_data.tunnel_id,
                                our_tunnel = %self.tunnel_id,
                                "TXT record belongs to different tunnel"
                            );
                            Ok((record.id.clone(), txt_data, false))
                        }
                    }
                    Err(e) => {
                        error!(
                            fqdn = %fqdn,
                            error = %e,
                            "could not parse TXT record content as JSON"
                        );
                        Err(Error::Cloudflare(format!(
                            "could not parse managed TXT content for '{fqdn}': {e}"
                        )))
                    }
                }
            }
            _ => {
                error!(
                    fqdn = %fqdn,
                    count = records.len(),
                    "multiple managed TXT records found"
                );
                Err(Error::Cloudflare(format!(
                    "multiple managed TXT records returned for '{fqdn}'"
                )))
            }
        }
    }

    /// Create or update a CNAME record for the given FQDN pointing to the tunnel.
    /// Returns the DNS record ID.
    pub async fn insert_or_update_cname(&self, fqdn: &str, dns_id: &str) -> Result<String> {
        let content = format!("{}.cfargotunnel.com", self.tunnel_id);

        if !dns_id.is_empty() {
            info!(fqdn = %fqdn, dns_id = %dns_id, "updating existing CNAME record");
            let url = format!(
                "{}/zones/{}/dns_records/{}",
                self.base_url, self.zone_id, dns_id
            );
            let body = UpdateDnsRecordRequest {
                record_type: "CNAME".to_string(),
                name: fqdn.to_string(),
                content,
                ttl: 1,
                proxied: true,
                comment: "Managed by cloudflare-operator".to_string(),
            };

            let resp = self
                .http
                .put(&url)
                .headers(self.auth_headers())
                .json(&body)
                .send()
                .await
                .map_err(|e| Error::Cloudflare(format!("HTTP error updating CNAME: {e}")))?;

            let cf_resp: CfResponse<DnsRecord> = resp
                .json()
                .await
                .map_err(|e| {
                    Error::Cloudflare(format!("error parsing update CNAME response: {e}"))
                })?;

            Self::parse_response(cf_resp)?;
            info!(fqdn = %fqdn, "CNAME record updated");
            Ok(dns_id.to_string())
        } else {
            info!(fqdn = %fqdn, "inserting new CNAME record");
            let url = format!("{}/zones/{}/dns_records", self.base_url, self.zone_id);
            let body = CreateDnsRecordRequest {
                record_type: "CNAME".to_string(),
                name: fqdn.to_string(),
                content,
                ttl: 1,
                proxied: true,
                comment: "Managed by cloudflare-operator".to_string(),
            };

            let resp = self
                .http
                .post(&url)
                .headers(self.auth_headers())
                .json(&body)
                .send()
                .await
                .map_err(|e| Error::Cloudflare(format!("HTTP error creating CNAME: {e}")))?;

            let cf_resp: CfResponse<DnsRecord> = resp
                .json()
                .await
                .map_err(|e| {
                    Error::Cloudflare(format!("error parsing create CNAME response: {e}"))
                })?;

            let record = Self::parse_response(cf_resp)?;
            info!(fqdn = %fqdn, dns_id = %record.id, "CNAME record created");
            Ok(record.id)
        }
    }

    /// Create or update a managed TXT record for the given FQDN.
    pub async fn insert_or_update_txt(
        &self,
        fqdn: &str,
        txt_id: &str,
        dns_id: &str,
    ) -> Result<()> {
        let txt_name = format!("{TXT_PREFIX}{fqdn}");
        let txt_content = serde_json::to_string(&DnsManagedRecordTxt {
            dns_id: dns_id.to_string(),
            tunnel_id: self.tunnel_id.clone(),
            tunnel_name: self.tunnel_name.clone(),
        })?;

        if !txt_id.is_empty() {
            info!(fqdn = %fqdn, txt_id = %txt_id, "updating existing TXT record");
            let url = format!(
                "{}/zones/{}/dns_records/{}",
                self.base_url, self.zone_id, txt_id
            );
            let body = UpdateDnsRecordRequest {
                record_type: "TXT".to_string(),
                name: txt_name,
                content: txt_content,
                ttl: 1,
                proxied: false,
                comment: "Managed by cloudflare-operator".to_string(),
            };

            let resp = self
                .http
                .put(&url)
                .headers(self.auth_headers())
                .json(&body)
                .send()
                .await
                .map_err(|e| Error::Cloudflare(format!("HTTP error updating TXT: {e}")))?;

            let cf_resp: CfResponse<DnsRecord> = resp
                .json()
                .await
                .map_err(|e| {
                    Error::Cloudflare(format!("error parsing update TXT response: {e}"))
                })?;

            Self::parse_response(cf_resp)?;
            info!(fqdn = %fqdn, "TXT record updated");
        } else {
            info!(fqdn = %fqdn, "inserting new TXT record");
            let url = format!("{}/zones/{}/dns_records", self.base_url, self.zone_id);
            let body = CreateDnsRecordRequest {
                record_type: "TXT".to_string(),
                name: txt_name,
                content: txt_content,
                ttl: 1,
                proxied: false,
                comment: "Managed by cloudflare-operator".to_string(),
            };

            let resp = self
                .http
                .post(&url)
                .headers(self.auth_headers())
                .json(&body)
                .send()
                .await
                .map_err(|e| Error::Cloudflare(format!("HTTP error creating TXT: {e}")))?;

            let cf_resp: CfResponse<DnsRecord> = resp
                .json()
                .await
                .map_err(|e| {
                    Error::Cloudflare(format!("error parsing create TXT response: {e}"))
                })?;

            Self::parse_response(cf_resp)?;
            info!(fqdn = %fqdn, "TXT record created");
        }

        Ok(())
    }

    /// Delete a DNS record by ID. Only deletes if `created` is true (i.e., we created it).
    pub async fn delete_dns_id(&self, fqdn: &str, dns_id: &str, created: bool) -> Result<()> {
        if !created {
            return Ok(());
        }

        let url = format!(
            "{}/zones/{}/dns_records/{}",
            self.base_url, self.zone_id, dns_id
        );
        let resp = self
            .http
            .delete(&url)
            .headers(self.auth_headers())
            .send()
            .await
            .map_err(|e| Error::Cloudflare(format!("HTTP error deleting DNS record: {e}")))?;

        let cf_resp: CfResponse<serde_json::Value> = resp
            .json()
            .await
            .map_err(|e| {
                Error::Cloudflare(format!("error parsing delete DNS response: {e}"))
            })?;

        if !cf_resp.success {
            let msgs: Vec<String> =
                cf_resp.errors.iter().map(|e| format!("[{}] {}", e.code, e.message)).collect();
            error!(
                fqdn = %fqdn,
                dns_id = %dns_id,
                errors = %msgs.join("; "),
                "error deleting DNS record"
            );
            return Err(Error::Cloudflare(msgs.join("; ")));
        }

        info!(fqdn = %fqdn, dns_id = %dns_id, "deleted DNS record");
        Ok(())
    }

    // ── Edge configuration ──────────────────────────────────────────────

    /// Push tunnel ingress configuration to Cloudflare's edge.
    pub async fn update_tunnel_configuration(
        &self,
        ingress: &[TunnelIngressRule],
    ) -> Result<()> {
        let url = format!(
            "{}/accounts/{}/cfd_tunnel/{}/configurations",
            self.base_url, self.account_id, self.tunnel_id
        );
        let body = TunnelConfigurationRequest {
            config: TunnelConfig {
                ingress: ingress.to_vec(),
            },
        };

        let resp = self
            .http
            .put(&url)
            .headers(self.auth_headers())
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                Error::Cloudflare(format!("HTTP error updating tunnel configuration: {e}"))
            })?;

        let cf_resp: CfResponse<serde_json::Value> = resp
            .json()
            .await
            .map_err(|e| {
                Error::Cloudflare(format!(
                    "error parsing tunnel configuration response: {e}"
                ))
            })?;

        if !cf_resp.success {
            let msgs: Vec<String> =
                cf_resp.errors.iter().map(|e| format!("[{}] {}", e.code, e.message)).collect();
            error!(
                tunnel_id = %self.tunnel_id,
                errors = %msgs.join("; "),
                "failed to update tunnel edge configuration"
            );
            return Err(Error::Cloudflare(msgs.join("; ")));
        }

        info!(
            tunnel_id = %self.tunnel_id,
            rule_count = ingress.len(),
            "updated tunnel edge configuration"
        );
        Ok(())
    }

    /// Push an empty configuration (catch-all 404) to the edge.
    pub async fn clear_tunnel_configuration(&self) -> Result<()> {
        self.update_tunnel_configuration(&[TunnelIngressRule {
            hostname: None,
            service: "http_status:404".to_string(),
            path: None,
        }])
        .await
    }

    // ── Internal helpers ────────────────────────────────────────────────

    /// List DNS records by type and name.
    async fn list_dns_records(&self, record_type: &str, name: &str) -> Result<Vec<DnsRecord>> {
        let url = format!("{}/zones/{}/dns_records", self.base_url, self.zone_id);
        let resp = self
            .http
            .get(&url)
            .headers(self.auth_headers())
            .query(&[("type", record_type), ("name", name)])
            .send()
            .await
            .map_err(|e| Error::Cloudflare(format!("HTTP error listing DNS records: {e}")))?;

        let cf_resp: CfListResponse<DnsRecord> = resp
            .json()
            .await
            .map_err(|e| {
                Error::Cloudflare(format!("error parsing DNS records list response: {e}"))
            })?;

        Self::parse_list_response(cf_resp)
    }
}
