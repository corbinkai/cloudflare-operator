package controller

import (
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/adyanth/cloudflare-operator/internal/clients/cf"
	"github.com/cloudflare/cloudflare-go"
	logrtesting "github.com/go-logr/logr/testr"
)

// cfMockHandler routes Cloudflare API requests to registered handler functions.
type cfMockHandler struct {
	handlers map[string]http.HandlerFunc
}

func (m *cfMockHandler) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	key := r.Method + " " + r.URL.Path
	if h, ok := m.handlers[key]; ok {
		h(w, r)
		return
	}
	// Fall through: return 404 for unhandled routes
	w.WriteHeader(http.StatusNotFound)
	fmt.Fprintf(w, `{"success":false,"errors":[{"message":"unhandled route: %s"}]}`, key)
}

// newCfAPIWithMock creates a real cf.API pointed at a mock HTTP server.
// The caller provides handler functions keyed by "METHOD /path".
func newCfAPIWithMock(t *testing.T, handlers map[string]http.HandlerFunc) (*cf.API, *httptest.Server) {
	t.Helper()
	mock := &cfMockHandler{handlers: handlers}
	server := httptest.NewServer(mock)

	// Create a cloudflare client pointed at our mock server
	client, err := cloudflare.NewWithAPIToken("fake-token", cloudflare.BaseURL(server.URL))
	if err != nil {
		t.Fatal(err)
	}

	api := &cf.API{
		Log:              logrtesting.New(t),
		ValidAccountId:   "acc-123",
		ValidTunnelId:    "tun-456",
		ValidTunnelName:  "test-tunnel",
		ValidZoneId:      "zone-789",
		Domain:           "example.com",
		CloudflareClient: client,
	}
	return api, server
}

// cfListResponse wraps DNS record list results in the Cloudflare API envelope.
func cfListResponse(records []cloudflare.DNSRecord) []byte {
	resp := struct {
		Success    bool                  `json:"success"`
		Errors     []interface{}         `json:"errors"`
		Messages   []interface{}         `json:"messages"`
		Result     []cloudflare.DNSRecord `json:"result"`
		ResultInfo cloudflare.ResultInfo  `json:"result_info"`
	}{
		Success:  true,
		Errors:   []interface{}{},
		Messages: []interface{}{},
		Result:   records,
		ResultInfo: cloudflare.ResultInfo{
			Page:    1,
			PerPage: 20,
			Count:   len(records),
		},
	}
	b, _ := json.Marshal(resp)
	return b
}

// cfDeleteResponse returns a success response for DNS record deletion.
func cfDeleteResponse(id string) []byte {
	resp := struct {
		Success  bool          `json:"success"`
		Errors   []interface{} `json:"errors"`
		Messages []interface{} `json:"messages"`
		Result   struct {
			ID string `json:"id"`
		} `json:"result"`
	}{
		Success:  true,
		Errors:   []interface{}{},
		Messages: []interface{}{},
	}
	resp.Result.ID = id
	b, _ := json.Marshal(resp)
	return b
}

func TestDeleteDNSForHostname_Success(t *testing.T) {
	txtContent, _ := json.Marshal(cf.DnsManagedRecordTxt{
		DnsId:      "dns-1",
		TunnelName: "test-tunnel",
		TunnelId:   "tun-456",
	})

	var deletedIDs []string
	handlers := map[string]http.HandlerFunc{
		// GetManagedDnsTxt calls ListDNSRecords for TXT type
		"GET /zones/zone-789/dns_records": func(w http.ResponseWriter, r *http.Request) {
			recordType := r.URL.Query().Get("type")
			name := r.URL.Query().Get("name")
			if recordType == "TXT" && name == "_managed.app.example.com" {
				records := []cloudflare.DNSRecord{
					{
						ID:      "txt-1",
						Type:    "TXT",
						Name:    "_managed.app.example.com",
						Content: string(txtContent),
					},
				}
				w.Header().Set("Content-Type", "application/json")
				w.Write(cfListResponse(records))
				return
			}
			// Should not be called for CNAME in deleteDNSForHostname
			w.Header().Set("Content-Type", "application/json")
			w.Write(cfListResponse(nil))
		},
		// DeleteDNSId calls DeleteDNSRecord
		"DELETE /zones/zone-789/dns_records/dns-1": func(w http.ResponseWriter, r *http.Request) {
			deletedIDs = append(deletedIDs, "dns-1")
			w.Header().Set("Content-Type", "application/json")
			w.Write(cfDeleteResponse("dns-1"))
		},
		"DELETE /zones/zone-789/dns_records/txt-1": func(w http.ResponseWriter, r *http.Request) {
			deletedIDs = append(deletedIDs, "txt-1")
			w.Header().Set("Content-Type", "application/json")
			w.Write(cfDeleteResponse("txt-1"))
		},
	}

	api, server := newCfAPIWithMock(t, handlers)
	defer server.Close()

	err := deleteDNSForHostname(api, logrtesting.New(t), "app.example.com")
	if err != nil {
		t.Fatalf("expected nil error, got %v", err)
	}

	if len(deletedIDs) != 2 {
		t.Fatalf("expected 2 deletes, got %d: %v", len(deletedIDs), deletedIDs)
	}
	// CNAME deleted first, then TXT
	if deletedIDs[0] != "dns-1" {
		t.Errorf("expected first delete to be dns-1, got %s", deletedIDs[0])
	}
	if deletedIDs[1] != "txt-1" {
		t.Errorf("expected second delete to be txt-1, got %s", deletedIDs[1])
	}
}

func TestDeleteDNSForHostname_DifferentTunnel(t *testing.T) {
	// TXT record exists but belongs to a different tunnel
	txtContent, _ := json.Marshal(cf.DnsManagedRecordTxt{
		DnsId:      "dns-other",
		TunnelName: "other-tunnel",
		TunnelId:   "tun-other",
	})

	handlers := map[string]http.HandlerFunc{
		"GET /zones/zone-789/dns_records": func(w http.ResponseWriter, r *http.Request) {
			records := []cloudflare.DNSRecord{
				{
					ID:      "txt-other",
					Type:    "TXT",
					Name:    "_managed.app.example.com",
					Content: string(txtContent),
				},
			}
			w.Header().Set("Content-Type", "application/json")
			w.Write(cfListResponse(records))
		},
	}

	api, server := newCfAPIWithMock(t, handlers)
	defer server.Close()

	err := deleteDNSForHostname(api, logrtesting.New(t), "app.example.com")
	if err != nil {
		t.Fatalf("expected nil error for different tunnel, got %v", err)
	}
}

func TestDeleteDNSForHostname_NoTXTRecord(t *testing.T) {
	handlers := map[string]http.HandlerFunc{
		"GET /zones/zone-789/dns_records": func(w http.ResponseWriter, r *http.Request) {
			// Return empty list — no TXT records found
			w.Header().Set("Content-Type", "application/json")
			w.Write(cfListResponse(nil))
		},
	}

	api, server := newCfAPIWithMock(t, handlers)
	defer server.Close()

	err := deleteDNSForHostname(api, logrtesting.New(t), "app.example.com")
	if err != nil {
		t.Fatalf("expected nil error for missing TXT, got %v", err)
	}
}

func TestDeleteDNSForHostname_PartialFailure(t *testing.T) {
	txtContent, _ := json.Marshal(cf.DnsManagedRecordTxt{
		DnsId:      "dns-1",
		TunnelName: "test-tunnel",
		TunnelId:   "tun-456",
	})

	var txtDeleted bool
	handlers := map[string]http.HandlerFunc{
		"GET /zones/zone-789/dns_records": func(w http.ResponseWriter, r *http.Request) {
			recordType := r.URL.Query().Get("type")
			if recordType == "TXT" {
				records := []cloudflare.DNSRecord{
					{
						ID:      "txt-1",
						Type:    "TXT",
						Name:    "_managed.app.example.com",
						Content: string(txtContent),
					},
				}
				w.Header().Set("Content-Type", "application/json")
				w.Write(cfListResponse(records))
				return
			}
			w.Header().Set("Content-Type", "application/json")
			w.Write(cfListResponse(nil))
		},
		// CNAME delete fails
		"DELETE /zones/zone-789/dns_records/dns-1": func(w http.ResponseWriter, r *http.Request) {
			w.WriteHeader(http.StatusInternalServerError)
			fmt.Fprint(w, `{"success":false,"errors":[{"message":"internal error"}]}`)
		},
		// TXT delete succeeds
		"DELETE /zones/zone-789/dns_records/txt-1": func(w http.ResponseWriter, r *http.Request) {
			txtDeleted = true
			w.Header().Set("Content-Type", "application/json")
			w.Write(cfDeleteResponse("txt-1"))
		},
	}

	api, server := newCfAPIWithMock(t, handlers)
	defer server.Close()

	err := deleteDNSForHostname(api, logrtesting.New(t), "app.example.com")
	if err == nil {
		t.Fatal("expected error for partial failure, got nil")
	}

	if !txtDeleted {
		t.Error("expected TXT record deletion to be attempted even when CNAME delete fails")
	}
}

// TestDeleteDNSForHostname_EmptyDnsId verifies that when the TXT record has an empty DnsId,
// only the TXT record itself is deleted (no CNAME delete is attempted).
func TestDeleteDNSForHostname_EmptyDnsId(t *testing.T) {
	txtContent, _ := json.Marshal(cf.DnsManagedRecordTxt{
		DnsId:      "",
		TunnelName: "test-tunnel",
		TunnelId:   "tun-456",
	})

	var txtDeleted bool
	handlers := map[string]http.HandlerFunc{
		"GET /zones/zone-789/dns_records": func(w http.ResponseWriter, r *http.Request) {
			recordType := r.URL.Query().Get("type")
			if recordType == "TXT" {
				records := []cloudflare.DNSRecord{
					{
						ID:      "txt-1",
						Type:    "TXT",
						Name:    "_managed.orphan.example.com",
						Content: string(txtContent),
					},
				}
				w.Header().Set("Content-Type", "application/json")
				w.Write(cfListResponse(records))
				return
			}
			w.Header().Set("Content-Type", "application/json")
			w.Write(cfListResponse(nil))
		},
		"DELETE /zones/zone-789/dns_records/txt-1": func(w http.ResponseWriter, r *http.Request) {
			txtDeleted = true
			w.Header().Set("Content-Type", "application/json")
			w.Write(cfDeleteResponse("txt-1"))
		},
	}

	api, server := newCfAPIWithMock(t, handlers)
	defer server.Close()

	err := deleteDNSForHostname(api, logrtesting.New(t), "orphan.example.com")
	if err != nil {
		t.Fatalf("expected nil error, got %v", err)
	}
	if !txtDeleted {
		t.Error("expected TXT record to be deleted")
	}
}

