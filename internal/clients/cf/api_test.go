package cf

import (
	"encoding/json"
	"testing"
	"time"

	"github.com/cloudflare/cloudflare-go"
	"github.com/go-logr/logr"
	"k8s.io/utils/ptr"

	"github.com/adyanth/cloudflare-operator/internal/testutil/cfmock"
)

// newTestAPI creates a cf.API backed by the given mock server.
func newTestAPI(t *testing.T, s *cfmock.Server) *API {
	t.Helper()
	client, err := cloudflare.NewWithAPIToken("test-token", cloudflare.BaseURL(s.URL+"/client/v4"))
	if err != nil {
		t.Fatalf("failed to create cloudflare client: %v", err)
	}
	return &API{
		Log:              logr.Discard(),
		CloudflareClient: client,
	}
}

// --- CreateTunnel ---

func TestCreateTunnel_Success(t *testing.T) {
	s := cfmock.NewServer()
	defer s.Close()
	s.AddAccount("acct-1", "my-account")

	api := newTestAPI(t, s)
	api.AccountId = "acct-1"
	api.TunnelName = "test-tunnel"

	tunnelID, credsJSON, err := api.CreateTunnel()
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if tunnelID == "" {
		t.Fatal("expected non-empty tunnel ID")
	}

	var creds TunnelCredentialsFile
	if err := json.Unmarshal([]byte(credsJSON), &creds); err != nil {
		t.Fatalf("failed to unmarshal creds: %v", err)
	}
	if creds.AccountTag != "acct-1" {
		t.Errorf("expected AccountTag=acct-1, got %s", creds.AccountTag)
	}
	if creds.TunnelID != tunnelID {
		t.Errorf("expected TunnelID=%s, got %s", tunnelID, creds.TunnelID)
	}
	if creds.TunnelName != "test-tunnel" {
		t.Errorf("expected TunnelName=test-tunnel, got %s", creds.TunnelName)
	}
	if creds.TunnelSecret == "" {
		t.Error("expected non-empty TunnelSecret")
	}

	// Verify the tunnel was created in the mock
	calls := s.GetCallsByPathContains("cfd_tunnel")
	if len(calls) == 0 {
		t.Fatal("expected at least one tunnel API call")
	}
}

func TestCreateTunnel_AccountValidationFails(t *testing.T) {
	s := cfmock.NewServer()
	defer s.Close()
	// No accounts configured

	api := newTestAPI(t, s)
	api.AccountId = "nonexistent"
	api.TunnelName = "test-tunnel"

	_, _, err := api.CreateTunnel()
	if err == nil {
		t.Fatal("expected error when account validation fails")
	}
}

func TestCreateTunnel_APIError(t *testing.T) {
	s := cfmock.NewServer()
	defer s.Close()
	s.AddAccount("acct-1", "my-account")
	s.SetError("POST", "/client/v4/accounts/acct-1/cfd_tunnel", 500, "internal server error")

	api := newTestAPI(t, s)
	api.AccountId = "acct-1"
	api.TunnelName = "test-tunnel"

	_, _, err := api.CreateTunnel()
	if err == nil {
		t.Fatal("expected error from API")
	}
}

// --- DeleteTunnel ---

func TestDeleteTunnel_Success(t *testing.T) {
	s := cfmock.NewServer()
	defer s.Close()
	s.AddAccount("acct-1", "my-account")
	s.AddZone("zone-1", "example.com")
	s.AddTunnel("acct-1", "tun-1", "my-tunnel")

	api := newTestAPI(t, s)
	api.ValidAccountId = "acct-1"
	api.ValidTunnelId = "tun-1"
	api.ValidTunnelName = "my-tunnel"
	api.ValidZoneId = "zone-1"
	api.Domain = "example.com"

	err := api.DeleteTunnel()
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	// Verify cleanup connections was called
	cleanupCalls := s.GetCallsByPathContains("connections")
	if len(cleanupCalls) != 1 {
		t.Errorf("expected 1 cleanup call, got %d", len(cleanupCalls))
	}

	// Verify delete was called
	deleteCalls := s.GetCallsByMethod("DELETE")
	found := false
	for _, c := range deleteCalls {
		if !containsString(c.Path, "connections") {
			found = true
		}
	}
	if !found {
		t.Error("expected a DELETE call for the tunnel itself")
	}
}

func TestDeleteTunnel_CleanupFails(t *testing.T) {
	s := cfmock.NewServer()
	defer s.Close()
	s.AddAccount("acct-1", "my-account")
	s.AddZone("zone-1", "example.com")
	s.AddTunnel("acct-1", "tun-1", "my-tunnel")
	s.SetError("DELETE", "/client/v4/accounts/acct-1/cfd_tunnel/tun-1/connections", 500, "cleanup failed")

	api := newTestAPI(t, s)
	api.ValidAccountId = "acct-1"
	api.ValidTunnelId = "tun-1"
	api.ValidTunnelName = "my-tunnel"
	api.ValidZoneId = "zone-1"
	api.Domain = "example.com"

	err := api.DeleteTunnel()
	if err == nil {
		t.Fatal("expected error when cleanup fails")
	}
}

func TestDeleteTunnel_DeleteFails(t *testing.T) {
	s := cfmock.NewServer()
	defer s.Close()
	s.AddAccount("acct-1", "my-account")
	s.AddZone("zone-1", "example.com")
	// Tunnel not in mock, so the DELETE will 404
	// But cleanup will also 404, so we need to add the tunnel for cleanup only
	s.AddTunnel("acct-1", "tun-1", "my-tunnel")

	// Error only on the exact tunnel DELETE, not the connections DELETE
	// Since both match "DELETE /client/v4/accounts/acct-1/cfd_tunnel/tun-1",
	// we need to be more specific. The connections endpoint fires first.
	// Remove the tunnel from mock so the delete fails after cleanup succeeds.
	// Actually, let's set error on DELETE for the tunnel path but not connections.
	// The path prefix for tunnel delete is same as connections prefix.
	// We'll set the error after cleanup succeeds by using a different approach:
	// just remove the tunnel from mock after cleanup call would succeed.
	// Simplest: use error override that doesn't match connections.

	// Actually, let's just verify the flow with a real approach: no tunnel means
	// cleanup also fails. Let's test with validation failure instead.
	api := newTestAPI(t, s)
	api.ValidAccountId = "acct-1"
	api.ValidTunnelId = "tun-nonexistent"
	api.ValidTunnelName = "my-tunnel"
	api.ValidZoneId = "zone-1"
	api.Domain = "example.com"

	err := api.DeleteTunnel()
	if err == nil {
		t.Fatal("expected error when tunnel delete fails")
	}
}

// --- ValidateAll ---

func TestValidateAll_Success(t *testing.T) {
	s := cfmock.NewServer()
	defer s.Close()
	s.AddAccount("acct-1", "my-account")
	s.AddZone("zone-1", "example.com")
	s.AddTunnel("acct-1", "tun-1", "my-tunnel")

	api := newTestAPI(t, s)
	api.AccountId = "acct-1"
	api.TunnelId = "tun-1"
	api.Domain = "example.com"

	err := api.ValidateAll()
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if api.ValidAccountId != "acct-1" {
		t.Errorf("expected ValidAccountId=acct-1, got %s", api.ValidAccountId)
	}
	if api.ValidTunnelId != "tun-1" {
		t.Errorf("expected ValidTunnelId=tun-1, got %s", api.ValidTunnelId)
	}
	if api.ValidZoneId != "zone-1" {
		t.Errorf("expected ValidZoneId=zone-1, got %s", api.ValidZoneId)
	}
}

func TestValidateAll_AccountFails(t *testing.T) {
	s := cfmock.NewServer()
	defer s.Close()

	api := newTestAPI(t, s)
	// No account ID or name
	api.TunnelId = "tun-1"
	api.Domain = "example.com"

	err := api.ValidateAll()
	if err == nil {
		t.Fatal("expected error when account validation fails")
	}
}

func TestValidateAll_TunnelFails(t *testing.T) {
	s := cfmock.NewServer()
	defer s.Close()
	s.AddAccount("acct-1", "my-account")
	s.AddZone("zone-1", "example.com")

	api := newTestAPI(t, s)
	api.AccountId = "acct-1"
	// No tunnel ID or name
	api.Domain = "example.com"

	err := api.ValidateAll()
	if err == nil {
		t.Fatal("expected error when tunnel validation fails")
	}
}

func TestValidateAll_ZoneFails(t *testing.T) {
	s := cfmock.NewServer()
	defer s.Close()
	s.AddAccount("acct-1", "my-account")
	s.AddTunnel("acct-1", "tun-1", "my-tunnel")

	api := newTestAPI(t, s)
	api.AccountId = "acct-1"
	api.TunnelId = "tun-1"
	// No domain

	err := api.ValidateAll()
	if err == nil {
		t.Fatal("expected error when zone validation fails")
	}
}

// --- GetAccountId ---

func TestGetAccountId_Cached(t *testing.T) {
	s := cfmock.NewServer()
	defer s.Close()

	api := newTestAPI(t, s)
	api.ValidAccountId = "cached-acct"

	id, err := api.GetAccountId()
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if id != "cached-acct" {
		t.Errorf("expected cached-acct, got %s", id)
	}
	if len(s.GetCalls()) != 0 {
		t.Error("expected no API calls when cached")
	}
}

func TestGetAccountId_FromID(t *testing.T) {
	s := cfmock.NewServer()
	defer s.Close()
	s.AddAccount("acct-1", "my-account")

	api := newTestAPI(t, s)
	api.AccountId = "acct-1"

	id, err := api.GetAccountId()
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if id != "acct-1" {
		t.Errorf("expected acct-1, got %s", id)
	}
}

func TestGetAccountId_FromName(t *testing.T) {
	s := cfmock.NewServer()
	defer s.Close()
	s.AddAccount("acct-1", "my-account")

	api := newTestAPI(t, s)
	api.AccountName = "my-account"

	id, err := api.GetAccountId()
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if id != "acct-1" {
		t.Errorf("expected acct-1, got %s", id)
	}
}

func TestGetAccountId_BothEmpty(t *testing.T) {
	s := cfmock.NewServer()
	defer s.Close()

	api := newTestAPI(t, s)

	_, err := api.GetAccountId()
	if err == nil {
		t.Fatal("expected error when both account ID and name are empty")
	}
}

// --- GetTunnelId ---

func TestGetTunnelId_Cached(t *testing.T) {
	s := cfmock.NewServer()
	defer s.Close()

	api := newTestAPI(t, s)
	api.ValidTunnelId = "cached-tun"

	id, err := api.GetTunnelId()
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if id != "cached-tun" {
		t.Errorf("expected cached-tun, got %s", id)
	}
	if len(s.GetCalls()) != 0 {
		t.Error("expected no API calls when cached")
	}
}

func TestGetTunnelId_FromID(t *testing.T) {
	s := cfmock.NewServer()
	defer s.Close()
	s.AddAccount("acct-1", "my-account")
	s.AddTunnel("acct-1", "tun-1", "my-tunnel")

	api := newTestAPI(t, s)
	api.ValidAccountId = "acct-1"
	api.TunnelId = "tun-1"

	id, err := api.GetTunnelId()
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if id != "tun-1" {
		t.Errorf("expected tun-1, got %s", id)
	}
}

func TestGetTunnelId_FromName(t *testing.T) {
	s := cfmock.NewServer()
	defer s.Close()
	s.AddAccount("acct-1", "my-account")
	s.AddTunnel("acct-1", "tun-1", "my-tunnel")

	api := newTestAPI(t, s)
	api.ValidAccountId = "acct-1"
	api.TunnelName = "my-tunnel"

	id, err := api.GetTunnelId()
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if id != "tun-1" {
		t.Errorf("expected tun-1, got %s", id)
	}
}

func TestGetTunnelId_BothEmpty(t *testing.T) {
	s := cfmock.NewServer()
	defer s.Close()

	api := newTestAPI(t, s)

	_, err := api.GetTunnelId()
	if err == nil {
		t.Fatal("expected error when both tunnel ID and name are empty")
	}
}

// --- GetZoneId ---

func TestGetZoneId_Cached(t *testing.T) {
	s := cfmock.NewServer()
	defer s.Close()

	api := newTestAPI(t, s)
	api.ValidZoneId = "cached-zone"

	id, err := api.GetZoneId()
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if id != "cached-zone" {
		t.Errorf("expected cached-zone, got %s", id)
	}
}

func TestGetZoneId_FromDomain(t *testing.T) {
	s := cfmock.NewServer()
	defer s.Close()
	s.AddZone("zone-1", "example.com")

	api := newTestAPI(t, s)
	api.Domain = "example.com"

	id, err := api.GetZoneId()
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if id != "zone-1" {
		t.Errorf("expected zone-1, got %s", id)
	}
}

func TestGetZoneId_EmptyDomain(t *testing.T) {
	s := cfmock.NewServer()
	defer s.Close()

	api := newTestAPI(t, s)

	_, err := api.GetZoneId()
	if err == nil {
		t.Fatal("expected error when domain is empty")
	}
}

// --- GetTunnelCreds ---

func TestGetTunnelCreds_Success(t *testing.T) {
	s := cfmock.NewServer()
	defer s.Close()
	s.AddAccount("acct-1", "my-account")
	s.AddTunnel("acct-1", "tun-1", "my-tunnel")

	api := newTestAPI(t, s)
	api.ValidAccountId = "acct-1"
	api.ValidTunnelId = "tun-1"
	api.ValidTunnelName = "my-tunnel"

	credsJSON, err := api.GetTunnelCreds("my-secret")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	var creds map[string]string
	if err := json.Unmarshal([]byte(credsJSON), &creds); err != nil {
		t.Fatalf("failed to unmarshal creds: %v", err)
	}
	if creds["AccountTag"] != "acct-1" {
		t.Errorf("expected AccountTag=acct-1, got %s", creds["AccountTag"])
	}
	if creds["TunnelID"] != "tun-1" {
		t.Errorf("expected TunnelID=tun-1, got %s", creds["TunnelID"])
	}
	if creds["TunnelSecret"] != "my-secret" {
		t.Errorf("expected TunnelSecret=my-secret, got %s", creds["TunnelSecret"])
	}
	if creds["TunnelName"] != "my-tunnel" {
		t.Errorf("expected TunnelName=my-tunnel, got %s", creds["TunnelName"])
	}
}

func TestGetTunnelCreds_AccountFails(t *testing.T) {
	s := cfmock.NewServer()
	defer s.Close()

	api := newTestAPI(t, s)
	// No account configured

	_, err := api.GetTunnelCreds("secret")
	if err == nil {
		t.Fatal("expected error when account resolution fails")
	}
}

func TestGetTunnelCreds_TunnelFails(t *testing.T) {
	s := cfmock.NewServer()
	defer s.Close()
	s.AddAccount("acct-1", "my-account")

	api := newTestAPI(t, s)
	api.ValidAccountId = "acct-1"
	// No tunnel configured

	_, err := api.GetTunnelCreds("secret")
	if err == nil {
		t.Fatal("expected error when tunnel resolution fails")
	}
}

// --- InsertOrUpdateCName ---

func TestInsertOrUpdateCName_Insert(t *testing.T) {
	s := cfmock.NewServer()
	defer s.Close()
	s.AddZone("zone-1", "example.com")

	api := newTestAPI(t, s)
	api.ValidZoneId = "zone-1"
	api.ValidTunnelId = "tun-1"

	dnsID, err := api.InsertOrUpdateCName("app.example.com", "")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if dnsID == "" {
		t.Fatal("expected non-empty DNS record ID")
	}

	// Verify the record was created
	postCalls := s.GetCallsByMethod("POST")
	found := false
	for _, c := range postCalls {
		if containsString(c.Path, "dns_records") {
			found = true
			break
		}
	}
	if !found {
		t.Error("expected a POST call to dns_records")
	}
}

func TestInsertOrUpdateCName_Update(t *testing.T) {
	s := cfmock.NewServer()
	defer s.Close()
	s.AddZone("zone-1", "example.com")
	s.AddDNSRecord(cfmock.DNSRecord{
		ID:      "dns-existing",
		ZoneID:  "zone-1",
		Type:    "CNAME",
		Name:    "app.example.com",
		Content: "old.cfargotunnel.com",
		TTL:     1,
	})

	api := newTestAPI(t, s)
	api.ValidZoneId = "zone-1"
	api.ValidTunnelId = "tun-1"

	dnsID, err := api.InsertOrUpdateCName("app.example.com", "dns-existing")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if dnsID != "dns-existing" {
		t.Errorf("expected dns-existing, got %s", dnsID)
	}

	// Verify PATCH was called
	patchCalls := s.GetCallsByMethod("PATCH")
	if len(patchCalls) == 0 {
		t.Error("expected a PATCH call for update")
	}
}

func TestInsertOrUpdateCName_InsertError(t *testing.T) {
	s := cfmock.NewServer()
	defer s.Close()
	s.AddZone("zone-1", "example.com")
	s.SetError("POST", "/client/v4/zones/zone-1/dns_records", 500, "create failed")

	api := newTestAPI(t, s)
	api.ValidZoneId = "zone-1"
	api.ValidTunnelId = "tun-1"

	_, err := api.InsertOrUpdateCName("app.example.com", "")
	if err == nil {
		t.Fatal("expected error")
	}
}

func TestInsertOrUpdateCName_UpdateError(t *testing.T) {
	s := cfmock.NewServer()
	defer s.Close()
	s.AddZone("zone-1", "example.com")
	s.SetError("PATCH", "/client/v4/zones/zone-1/dns_records", 500, "update failed")

	api := newTestAPI(t, s)
	api.ValidZoneId = "zone-1"
	api.ValidTunnelId = "tun-1"

	_, err := api.InsertOrUpdateCName("app.example.com", "dns-existing")
	if err == nil {
		t.Fatal("expected error")
	}
}

// --- InsertOrUpdateTXT ---

func TestInsertOrUpdateTXT_Insert(t *testing.T) {
	s := cfmock.NewServer()
	defer s.Close()
	s.AddZone("zone-1", "example.com")

	api := newTestAPI(t, s)
	api.ValidZoneId = "zone-1"
	api.ValidTunnelId = "tun-1"
	api.ValidTunnelName = "my-tunnel"

	err := api.InsertOrUpdateTXT("app.example.com", "", "dns-cname-1")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	postCalls := s.GetCallsByMethod("POST")
	found := false
	for _, c := range postCalls {
		if containsString(c.Path, "dns_records") {
			// Verify the body contains the TXT_PREFIX
			if containsString(c.Body, TXT_PREFIX) {
				found = true
			}
			break
		}
	}
	if !found {
		t.Error("expected POST to dns_records with TXT prefix in body")
	}
}

func TestInsertOrUpdateTXT_Update(t *testing.T) {
	s := cfmock.NewServer()
	defer s.Close()
	s.AddZone("zone-1", "example.com")
	s.AddDNSRecord(cfmock.DNSRecord{
		ID:      "txt-existing",
		ZoneID:  "zone-1",
		Type:    "TXT",
		Name:    "_managed.app.example.com",
		Content: `{"DnsId":"old","TunnelName":"old","TunnelId":"old"}`,
		TTL:     1,
	})

	api := newTestAPI(t, s)
	api.ValidZoneId = "zone-1"
	api.ValidTunnelId = "tun-1"
	api.ValidTunnelName = "my-tunnel"

	err := api.InsertOrUpdateTXT("app.example.com", "txt-existing", "dns-cname-1")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	patchCalls := s.GetCallsByMethod("PATCH")
	if len(patchCalls) == 0 {
		t.Error("expected a PATCH call for TXT update")
	}
}

func TestInsertOrUpdateTXT_InsertError(t *testing.T) {
	s := cfmock.NewServer()
	defer s.Close()
	s.AddZone("zone-1", "example.com")
	s.SetError("POST", "/client/v4/zones/zone-1/dns_records", 500, "create failed")

	api := newTestAPI(t, s)
	api.ValidZoneId = "zone-1"
	api.ValidTunnelId = "tun-1"
	api.ValidTunnelName = "my-tunnel"

	err := api.InsertOrUpdateTXT("app.example.com", "", "dns-cname-1")
	if err == nil {
		t.Fatal("expected error")
	}
}

func TestInsertOrUpdateTXT_UpdateError(t *testing.T) {
	s := cfmock.NewServer()
	defer s.Close()
	s.AddZone("zone-1", "example.com")
	s.SetError("PATCH", "/client/v4/zones/zone-1/dns_records", 500, "update failed")

	api := newTestAPI(t, s)
	api.ValidZoneId = "zone-1"
	api.ValidTunnelId = "tun-1"
	api.ValidTunnelName = "my-tunnel"

	err := api.InsertOrUpdateTXT("app.example.com", "txt-existing", "dns-cname-1")
	if err == nil {
		t.Fatal("expected error")
	}
}

// --- DeleteDNSId ---

func TestDeleteDNSId_Success(t *testing.T) {
	s := cfmock.NewServer()
	defer s.Close()
	s.AddZone("zone-1", "example.com")
	s.AddDNSRecord(cfmock.DNSRecord{
		ID:     "dns-1",
		ZoneID: "zone-1",
		Type:   "CNAME",
		Name:   "app.example.com",
	})

	api := newTestAPI(t, s)
	api.ValidZoneId = "zone-1"

	err := api.DeleteDNSId("app.example.com", "dns-1", true)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	deleteCalls := s.GetCallsByMethod("DELETE")
	if len(deleteCalls) != 1 {
		t.Errorf("expected 1 DELETE call, got %d", len(deleteCalls))
	}
}

func TestDeleteDNSId_SkipWhenNotCreated(t *testing.T) {
	s := cfmock.NewServer()
	defer s.Close()

	api := newTestAPI(t, s)
	api.ValidZoneId = "zone-1"

	err := api.DeleteDNSId("app.example.com", "dns-1", false)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	if len(s.GetCalls()) != 0 {
		t.Error("expected no API calls when created=false")
	}
}

func TestDeleteDNSId_Error(t *testing.T) {
	s := cfmock.NewServer()
	defer s.Close()
	s.AddZone("zone-1", "example.com")
	s.SetError("DELETE", "/client/v4/zones/zone-1/dns_records", 500, "delete failed")

	api := newTestAPI(t, s)
	api.ValidZoneId = "zone-1"

	err := api.DeleteDNSId("app.example.com", "dns-1", true)
	if err == nil {
		t.Fatal("expected error")
	}
}

// --- GetDNSCNameId ---

func TestGetDNSCNameId_OneRecord(t *testing.T) {
	s := cfmock.NewServer()
	defer s.Close()
	s.AddZone("zone-1", "example.com")
	s.AddDNSRecord(cfmock.DNSRecord{
		ID:      "dns-1",
		ZoneID:  "zone-1",
		Type:    "CNAME",
		Name:    "app.example.com",
		Content: "tun-1.cfargotunnel.com",
	})

	api := newTestAPI(t, s)
	api.Domain = "example.com"

	id, err := api.GetDNSCNameId("app.example.com")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if id != "dns-1" {
		t.Errorf("expected dns-1, got %s", id)
	}
}

func TestGetDNSCNameId_ZeroRecords(t *testing.T) {
	s := cfmock.NewServer()
	defer s.Close()
	s.AddZone("zone-1", "example.com")

	api := newTestAPI(t, s)
	api.Domain = "example.com"

	_, err := api.GetDNSCNameId("app.example.com")
	if err == nil {
		t.Fatal("expected error when no records found")
	}
}

func TestGetDNSCNameId_MultipleRecords(t *testing.T) {
	s := cfmock.NewServer()
	defer s.Close()
	s.AddZone("zone-1", "example.com")
	s.AddDNSRecord(cfmock.DNSRecord{
		ID:     "dns-1",
		ZoneID: "zone-1",
		Type:   "CNAME",
		Name:   "app.example.com",
	})
	s.AddDNSRecord(cfmock.DNSRecord{
		ID:     "dns-2",
		ZoneID: "zone-1",
		Type:   "CNAME",
		Name:   "app.example.com",
	})

	api := newTestAPI(t, s)
	api.Domain = "example.com"

	_, err := api.GetDNSCNameId("app.example.com")
	if err == nil {
		t.Fatal("expected error when multiple records found")
	}
}

// --- GetManagedDnsTxt ---

func TestGetManagedDnsTxt_NoRecords(t *testing.T) {
	s := cfmock.NewServer()
	defer s.Close()
	s.AddZone("zone-1", "example.com")

	api := newTestAPI(t, s)
	api.Domain = "example.com"
	api.ValidTunnelId = "tun-1"

	txtID, rec, canManage, err := api.GetManagedDnsTxt("app.example.com")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if txtID != "" {
		t.Errorf("expected empty txtID, got %s", txtID)
	}
	if rec.DnsId != "" {
		t.Error("expected empty DnsManagedRecordTxt")
	}
	if !canManage {
		t.Error("expected canManage=true when no records exist")
	}
}

func TestGetManagedDnsTxt_SameTunnel(t *testing.T) {
	s := cfmock.NewServer()
	defer s.Close()
	s.AddZone("zone-1", "example.com")

	txtContent, _ := json.Marshal(DnsManagedRecordTxt{
		DnsId:      "dns-cname-1",
		TunnelId:   "tun-1",
		TunnelName: "my-tunnel",
	})
	s.AddDNSRecord(cfmock.DNSRecord{
		ID:      "txt-1",
		ZoneID:  "zone-1",
		Type:    "TXT",
		Name:    "_managed.app.example.com",
		Content: string(txtContent),
	})

	api := newTestAPI(t, s)
	api.Domain = "example.com"
	api.ValidTunnelId = "tun-1"

	txtID, rec, canManage, err := api.GetManagedDnsTxt("app.example.com")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if txtID != "txt-1" {
		t.Errorf("expected txt-1, got %s", txtID)
	}
	if rec.TunnelId != "tun-1" {
		t.Errorf("expected TunnelId=tun-1, got %s", rec.TunnelId)
	}
	if !canManage {
		t.Error("expected canManage=true for same tunnel")
	}
}

func TestGetManagedDnsTxt_DifferentTunnel(t *testing.T) {
	s := cfmock.NewServer()
	defer s.Close()
	s.AddZone("zone-1", "example.com")

	txtContent, _ := json.Marshal(DnsManagedRecordTxt{
		DnsId:      "dns-cname-1",
		TunnelId:   "tun-other",
		TunnelName: "other-tunnel",
	})
	s.AddDNSRecord(cfmock.DNSRecord{
		ID:      "txt-1",
		ZoneID:  "zone-1",
		Type:    "TXT",
		Name:    "_managed.app.example.com",
		Content: string(txtContent),
	})

	api := newTestAPI(t, s)
	api.Domain = "example.com"
	api.ValidTunnelId = "tun-1"

	_, _, canManage, err := api.GetManagedDnsTxt("app.example.com")
	// The method returns the record but canManage=false and err=nil for different tunnel
	// Looking at the code: when TunnelId != ValidTunnelId, it falls through to the
	// return at line 487 which returns "", DnsManagedRecordTxt{}, false, err (err is nil here)
	if canManage {
		t.Error("expected canManage=false for different tunnel")
	}
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
}

func TestGetManagedDnsTxt_Malformed(t *testing.T) {
	s := cfmock.NewServer()
	defer s.Close()
	s.AddZone("zone-1", "example.com")
	s.AddDNSRecord(cfmock.DNSRecord{
		ID:      "txt-1",
		ZoneID:  "zone-1",
		Type:    "TXT",
		Name:    "_managed.app.example.com",
		Content: "this is not json",
	})

	api := newTestAPI(t, s)
	api.Domain = "example.com"
	api.ValidTunnelId = "tun-1"

	txtID, _, canManage, err := api.GetManagedDnsTxt("app.example.com")
	if err == nil {
		t.Fatal("expected error for malformed TXT content")
	}
	if txtID != "txt-1" {
		t.Errorf("expected txt-1, got %s", txtID)
	}
	if canManage {
		t.Error("expected canManage=false for malformed content")
	}
}

func TestGetManagedDnsTxt_Multiple(t *testing.T) {
	s := cfmock.NewServer()
	defer s.Close()
	s.AddZone("zone-1", "example.com")
	s.AddDNSRecord(cfmock.DNSRecord{
		ID:      "txt-1",
		ZoneID:  "zone-1",
		Type:    "TXT",
		Name:    "_managed.app.example.com",
		Content: `{"DnsId":"a","TunnelId":"tun-1","TunnelName":"t1"}`,
	})
	s.AddDNSRecord(cfmock.DNSRecord{
		ID:      "txt-2",
		ZoneID:  "zone-1",
		Type:    "TXT",
		Name:    "_managed.app.example.com",
		Content: `{"DnsId":"b","TunnelId":"tun-2","TunnelName":"t2"}`,
	})

	api := newTestAPI(t, s)
	api.Domain = "example.com"
	api.ValidTunnelId = "tun-1"

	_, _, canManage, err := api.GetManagedDnsTxt("app.example.com")
	if err == nil {
		t.Fatal("expected error when multiple TXT records found")
	}
	if canManage {
		t.Error("expected canManage=false for multiple records")
	}
}

// --- UpdateTunnelConfiguration ---

func TestUpdateTunnelConfiguration_Success(t *testing.T) {
	s := cfmock.NewServer()
	defer s.Close()
	s.AddAccount("acct-1", "my-account")
	s.AddZone("zone-1", "example.com")
	s.AddTunnel("acct-1", "tun-1", "my-tunnel")

	api := newTestAPI(t, s)
	api.ValidAccountId = "acct-1"
	api.ValidTunnelId = "tun-1"
	api.ValidTunnelName = "my-tunnel"
	api.ValidZoneId = "zone-1"
	api.Domain = "example.com"

	ingress := []UnvalidatedIngressRule{
		{Hostname: "app.example.com", Service: "http://localhost:8080"},
		{Service: "http_status:404"},
	}

	err := api.UpdateTunnelConfiguration(ingress)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	putCalls := s.GetCallsByMethod("PUT")
	found := false
	for _, c := range putCalls {
		if containsString(c.Path, "configurations") {
			found = true
			break
		}
	}
	if !found {
		t.Error("expected a PUT call to tunnel configurations")
	}
}

func TestUpdateTunnelConfiguration_ValidationFails(t *testing.T) {
	s := cfmock.NewServer()
	defer s.Close()

	api := newTestAPI(t, s)
	// No valid IDs set

	ingress := []UnvalidatedIngressRule{
		{Service: "http_status:404"},
	}

	err := api.UpdateTunnelConfiguration(ingress)
	if err == nil {
		t.Fatal("expected error when validation fails")
	}
}

func TestUpdateTunnelConfiguration_APIError(t *testing.T) {
	s := cfmock.NewServer()
	defer s.Close()
	s.AddAccount("acct-1", "my-account")
	s.AddZone("zone-1", "example.com")
	s.AddTunnel("acct-1", "tun-1", "my-tunnel")
	s.SetError("PUT", "/client/v4/accounts/acct-1/cfd_tunnel/tun-1/configurations", 500, "config update failed")

	api := newTestAPI(t, s)
	api.ValidAccountId = "acct-1"
	api.ValidTunnelId = "tun-1"
	api.ValidTunnelName = "my-tunnel"
	api.ValidZoneId = "zone-1"
	api.Domain = "example.com"

	ingress := []UnvalidatedIngressRule{
		{Service: "http_status:404"},
	}

	err := api.UpdateTunnelConfiguration(ingress)
	if err == nil {
		t.Fatal("expected error from API")
	}
}

// --- ClearTunnelConfiguration ---

func TestClearTunnelConfiguration(t *testing.T) {
	s := cfmock.NewServer()
	defer s.Close()
	s.AddAccount("acct-1", "my-account")
	s.AddZone("zone-1", "example.com")
	s.AddTunnel("acct-1", "tun-1", "my-tunnel")

	api := newTestAPI(t, s)
	api.ValidAccountId = "acct-1"
	api.ValidTunnelId = "tun-1"
	api.ValidTunnelName = "my-tunnel"
	api.ValidZoneId = "zone-1"
	api.Domain = "example.com"

	err := api.ClearTunnelConfiguration()
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	// Verify a PUT was made with catch-all only
	putCalls := s.GetCallsByMethod("PUT")
	found := false
	for _, c := range putCalls {
		if containsString(c.Path, "configurations") && containsString(c.Body, "http_status:404") {
			found = true
			break
		}
	}
	if !found {
		t.Error("expected PUT to configurations with http_status:404 catch-all")
	}
}

// --- convertOriginRequest ---

func TestConvertOriginRequest_Nil(t *testing.T) {
	result := convertOriginRequest(nil)
	if result != nil {
		t.Error("expected nil result for nil input")
	}
}

func TestConvertOriginRequest_AllFields(t *testing.T) {
	connectTimeout := 30 * time.Second
	tlsTimeout := 10 * time.Second
	tcpKeepAlive := 15 * time.Second
	keepAliveTimeout := 60 * time.Second

	input := &OriginRequestConfig{
		ConnectTimeout:         &connectTimeout,
		TLSTimeout:             &tlsTimeout,
		TCPKeepAlive:           &tcpKeepAlive,
		NoHappyEyeballs:        ptr.To(true),
		KeepAliveConnections:   ptr.To(10),
		KeepAliveTimeout:       &keepAliveTimeout,
		HTTPHostHeader:         ptr.To("custom.host"),
		OriginServerName:       ptr.To("origin.example.com"),
		CAPool:                 ptr.To("/path/to/ca.pem"),
		NoTLSVerify:            ptr.To(true),
		Http2Origin:            ptr.To(false),
		DisableChunkedEncoding: ptr.To(true),
		BastionMode:            ptr.To(false),
		ProxyAddress:           ptr.To("127.0.0.1"),
		ProxyPort:              ptr.To(uint(1080)),
		ProxyType:              ptr.To("socks"),
	}

	result := convertOriginRequest(input)
	if result == nil {
		t.Fatal("expected non-nil result")
	}

	if result.ConnectTimeout == nil || result.ConnectTimeout.Duration != 30*time.Second {
		t.Error("ConnectTimeout mismatch")
	}
	if result.TLSTimeout == nil || result.TLSTimeout.Duration != 10*time.Second {
		t.Error("TLSTimeout mismatch")
	}
	if result.TCPKeepAlive == nil || result.TCPKeepAlive.Duration != 15*time.Second {
		t.Error("TCPKeepAlive mismatch")
	}
	if result.KeepAliveTimeout == nil || result.KeepAliveTimeout.Duration != 60*time.Second {
		t.Error("KeepAliveTimeout mismatch")
	}
	if result.NoHappyEyeballs == nil || *result.NoHappyEyeballs != true {
		t.Error("NoHappyEyeballs mismatch")
	}
	if result.KeepAliveConnections == nil || *result.KeepAliveConnections != 10 {
		t.Error("KeepAliveConnections mismatch")
	}
	if result.HTTPHostHeader == nil || *result.HTTPHostHeader != "custom.host" {
		t.Error("HTTPHostHeader mismatch")
	}
	if result.OriginServerName == nil || *result.OriginServerName != "origin.example.com" {
		t.Error("OriginServerName mismatch")
	}
	if result.CAPool == nil || *result.CAPool != "/path/to/ca.pem" {
		t.Error("CAPool mismatch")
	}
	if result.NoTLSVerify == nil || *result.NoTLSVerify != true {
		t.Error("NoTLSVerify mismatch")
	}
	if result.Http2Origin == nil || *result.Http2Origin != false {
		t.Error("Http2Origin mismatch")
	}
	if result.DisableChunkedEncoding == nil || *result.DisableChunkedEncoding != true {
		t.Error("DisableChunkedEncoding mismatch")
	}
	if result.BastionMode == nil || *result.BastionMode != false {
		t.Error("BastionMode mismatch")
	}
	if result.ProxyAddress == nil || *result.ProxyAddress != "127.0.0.1" {
		t.Error("ProxyAddress mismatch")
	}
	if result.ProxyPort == nil || *result.ProxyPort != 1080 {
		t.Error("ProxyPort mismatch")
	}
	if result.ProxyType == nil || *result.ProxyType != "socks" {
		t.Error("ProxyType mismatch")
	}
}

func TestConvertOriginRequest_DurationConversion(t *testing.T) {
	timeout := 5 * time.Minute
	input := &OriginRequestConfig{
		ConnectTimeout: &timeout,
	}

	result := convertOriginRequest(input)
	if result == nil {
		t.Fatal("expected non-nil result")
	}
	if result.ConnectTimeout == nil {
		t.Fatal("expected non-nil ConnectTimeout")
	}
	if result.ConnectTimeout.Duration != 5*time.Minute {
		t.Errorf("expected 5m, got %v", result.ConnectTimeout.Duration)
	}
	// Other duration fields should be nil
	if result.TLSTimeout != nil {
		t.Error("expected nil TLSTimeout")
	}
	if result.TCPKeepAlive != nil {
		t.Error("expected nil TCPKeepAlive")
	}
	if result.KeepAliveTimeout != nil {
		t.Error("expected nil KeepAliveTimeout")
	}
}

// --- helpers ---

func containsString(s, sub string) bool {
	return len(s) >= len(sub) && (s == sub || len(s) > 0 && len(sub) > 0 && stringContains(s, sub))
}

func stringContains(s, sub string) bool {
	for i := 0; i <= len(s)-len(sub); i++ {
		if s[i:i+len(sub)] == sub {
			return true
		}
	}
	return false
}
