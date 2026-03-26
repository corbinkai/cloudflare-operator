package cfmock

import (
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
)

// APICall records a single API call made to the mock server.
type APICall struct {
	Method string
	Path   string
	Query  map[string]string
	Body   string
}

// Tunnel represents a tunnel stored in the mock.
type Tunnel struct {
	ID        string `json:"id"`
	Name      string `json:"name"`
	Secret    string `json:"tunnel_secret,omitempty"`
	ConfigSrc string `json:"config_src,omitempty"`
}

// DNSRecord represents a DNS record stored in the mock.
type DNSRecord struct {
	ID      string  `json:"id"`
	ZoneID  string  `json:"zone_id"`
	Type    string  `json:"type"`
	Name    string  `json:"name"`
	Content string  `json:"content"`
	TTL     int     `json:"ttl"`
	Proxied *bool   `json:"proxied,omitempty"`
	Comment *string `json:"comment,omitempty"`
}

// Zone represents a zone stored in the mock.
type Zone struct {
	ID   string `json:"id"`
	Name string `json:"name"`
}

// Account represents an account stored in the mock.
type Account struct {
	ID   string `json:"id"`
	Name string `json:"name"`
}

// TunnelConfig represents a stored tunnel configuration.
type TunnelConfig struct {
	TunnelID string          `json:"tunnel_id"`
	Config   json.RawMessage `json:"config"`
}

// ErrorOverride configures an error response for a specific method+path prefix.
type ErrorOverride struct {
	Method     string
	PathPrefix string
	StatusCode int
	Message    string
}

// Server is a mock Cloudflare API server for testing.
type Server struct {
	mu             sync.Mutex
	HTTPServer     *httptest.Server
	URL            string
	Tunnels        map[string]Tunnel    // keyed by tunnel ID
	DNSRecords     map[string]DNSRecord // keyed by record ID
	Zones          map[string]Zone      // keyed by zone ID
	Accounts       map[string]Account   // keyed by account ID
	TunnelConfigs  map[string]TunnelConfig
	Calls          []APICall
	Errors         []ErrorOverride
	nextDNSID      int
	nextTunnelID   int
	nextZoneID     int
	nextAccountID  int
}

// NewServer creates a new mock Cloudflare API server and starts it.
func NewServer() *Server {
	s := &Server{
		Tunnels:       make(map[string]Tunnel),
		DNSRecords:    make(map[string]DNSRecord),
		Zones:         make(map[string]Zone),
		Accounts:      make(map[string]Account),
		TunnelConfigs: make(map[string]TunnelConfig),
	}
	s.HTTPServer = httptest.NewServer(http.HandlerFunc(s.handler))
	s.URL = s.HTTPServer.URL
	return s
}

// Close shuts down the mock server.
func (s *Server) Close() {
	s.HTTPServer.Close()
}

// AddAccount pre-configures an account.
func (s *Server) AddAccount(id, name string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.Accounts[id] = Account{ID: id, Name: name}
}

// AddZone pre-configures a zone.
func (s *Server) AddZone(id, name string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.Zones[id] = Zone{ID: id, Name: name}
}

// AddTunnel pre-configures a tunnel.
func (s *Server) AddTunnel(accountID, id, name string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.Tunnels[id] = Tunnel{ID: id, Name: name}
}

// AddDNSRecord pre-configures a DNS record.
func (s *Server) AddDNSRecord(rec DNSRecord) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.DNSRecords[rec.ID] = rec
}

// SetError configures an error response for requests matching method and path prefix.
func (s *Server) SetError(method, pathPrefix string, statusCode int, message string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.Errors = append(s.Errors, ErrorOverride{
		Method:     method,
		PathPrefix: pathPrefix,
		StatusCode: statusCode,
		Message:    message,
	})
}

// ClearErrors removes all error overrides.
func (s *Server) ClearErrors() {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.Errors = nil
}

// GetCalls returns a copy of all recorded API calls.
func (s *Server) GetCalls() []APICall {
	s.mu.Lock()
	defer s.mu.Unlock()
	out := make([]APICall, len(s.Calls))
	copy(out, s.Calls)
	return out
}

// GetCallsByMethod returns calls filtered by HTTP method.
func (s *Server) GetCallsByMethod(method string) []APICall {
	s.mu.Lock()
	defer s.mu.Unlock()
	var out []APICall
	for _, c := range s.Calls {
		if c.Method == method {
			out = append(out, c)
		}
	}
	return out
}

// GetCallsByPathContains returns calls where path contains the given substring.
func (s *Server) GetCallsByPathContains(sub string) []APICall {
	s.mu.Lock()
	defer s.mu.Unlock()
	var out []APICall
	for _, c := range s.Calls {
		if strings.Contains(c.Path, sub) {
			out = append(out, c)
		}
	}
	return out
}

func (s *Server) nextID(prefix string, counter *int) string {
	*counter++
	return fmt.Sprintf("%s-%d", prefix, *counter)
}

func (s *Server) handler(w http.ResponseWriter, r *http.Request) {
	s.mu.Lock()

	// Read body
	bodyBytes, _ := io.ReadAll(r.Body)
	body := string(bodyBytes)

	// Parse query params
	query := make(map[string]string)
	for k, v := range r.URL.Query() {
		if len(v) > 0 {
			query[k] = v[0]
		}
	}

	s.Calls = append(s.Calls, APICall{
		Method: r.Method,
		Path:   r.URL.Path,
		Query:  query,
		Body:   body,
	})

	// Check error overrides
	for _, eo := range s.Errors {
		if eo.Method == r.Method && strings.HasPrefix(r.URL.Path, eo.PathPrefix) {
			s.mu.Unlock()
			writeError(w, eo.StatusCode, eo.Message)
			return
		}
	}

	s.mu.Unlock()

	path := r.URL.Path

	// Strip /client/v4 prefix
	path = strings.TrimPrefix(path, "/client/v4")

	// Route the request
	switch {
	// PUT /accounts/{id}/cfd_tunnel/{id}/configurations
	case r.Method == http.MethodPut && matchPath(path, "/accounts/*/cfd_tunnel/*/configurations"):
		s.handleUpdateTunnelConfiguration(w, r, path, body)

	// DELETE /accounts/{id}/cfd_tunnel/{id}/connections
	case r.Method == http.MethodDelete && matchPath(path, "/accounts/*/cfd_tunnel/*/connections"):
		s.handleCleanupTunnelConnections(w, path)

	// POST /accounts/{id}/cfd_tunnel
	case r.Method == http.MethodPost && matchPath(path, "/accounts/*/cfd_tunnel"):
		s.handleCreateTunnel(w, body)

	// DELETE /accounts/{id}/cfd_tunnel/{id}
	case r.Method == http.MethodDelete && matchPath(path, "/accounts/*/cfd_tunnel/*"):
		s.handleDeleteTunnel(w, path)

	// GET /accounts/{id}/cfd_tunnel/{id}
	case r.Method == http.MethodGet && matchPath(path, "/accounts/*/cfd_tunnel/*"):
		s.handleGetTunnel(w, path)

	// GET /accounts/{id}/cfd_tunnel
	case r.Method == http.MethodGet && matchPath(path, "/accounts/*/cfd_tunnel"):
		s.handleListTunnels(w, r)

	// GET /accounts/{id} (single account)
	case r.Method == http.MethodGet && matchPath(path, "/accounts/*"):
		s.handleGetAccount(w, path)

	// GET /accounts
	case r.Method == http.MethodGet && path == "/accounts":
		s.handleListAccounts(w, r)

	// GET /zones/{id}/dns_records
	case r.Method == http.MethodGet && matchPath(path, "/zones/*/dns_records") && !matchPath(path, "/zones/*/dns_records/*"):
		s.handleListDNSRecords(w, r, path)

	// POST /zones/{id}/dns_records
	case r.Method == http.MethodPost && matchPath(path, "/zones/*/dns_records"):
		s.handleCreateDNSRecord(w, path, body)

	// PATCH /zones/{id}/dns_records/{id}
	case r.Method == http.MethodPatch && matchPath(path, "/zones/*/dns_records/*"):
		s.handleUpdateDNSRecord(w, path, body)

	// DELETE /zones/{id}/dns_records/{id}
	case r.Method == http.MethodDelete && matchPath(path, "/zones/*/dns_records/*"):
		s.handleDeleteDNSRecord(w, path)

	// GET /zones
	case r.Method == http.MethodGet && path == "/zones":
		s.handleListZones(w, r)

	default:
		writeError(w, http.StatusNotFound, fmt.Sprintf("no mock handler for %s %s", r.Method, r.URL.Path))
	}
}

// matchPath matches a URL path against a pattern where * matches a single path segment.
func matchPath(path, pattern string) bool {
	pathParts := strings.Split(strings.Trim(path, "/"), "/")
	patternParts := strings.Split(strings.Trim(pattern, "/"), "/")
	if len(pathParts) != len(patternParts) {
		return false
	}
	for i := range patternParts {
		if patternParts[i] == "*" {
			continue
		}
		if patternParts[i] != pathParts[i] {
			return false
		}
	}
	return true
}

// extractSegment extracts the nth path segment (0-indexed) from a path like /a/b/c/d.
func extractSegment(path string, index int) string {
	parts := strings.Split(strings.Trim(path, "/"), "/")
	if index >= len(parts) {
		return ""
	}
	return parts[index]
}

func writeJSON(w http.ResponseWriter, statusCode int, result interface{}) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(statusCode)
	resp := map[string]interface{}{
		"success":  true,
		"errors":   []interface{}{},
		"messages": []interface{}{},
		"result":   result,
	}
	json.NewEncoder(w).Encode(resp)
}

func writeJSONList(w http.ResponseWriter, statusCode int, result interface{}, count int) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(statusCode)
	resp := map[string]interface{}{
		"success":  true,
		"errors":   []interface{}{},
		"messages": []interface{}{},
		"result":   result,
		"result_info": map[string]interface{}{
			"page":        1,
			"per_page":    50,
			"total_pages": 1,
			"count":       count,
			"total_count": count,
		},
	}
	json.NewEncoder(w).Encode(resp)
}

func writeError(w http.ResponseWriter, statusCode int, message string) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(statusCode)
	resp := map[string]interface{}{
		"success": false,
		"errors": []interface{}{
			map[string]interface{}{
				"code":    statusCode,
				"message": message,
			},
		},
		"messages": []interface{}{},
		"result":   nil,
	}
	json.NewEncoder(w).Encode(resp)
}

// --- Tunnel handlers ---

func (s *Server) handleCreateTunnel(w http.ResponseWriter, body string) {
	var req struct {
		Name      string `json:"name"`
		Secret    string `json:"tunnel_secret"`
		ConfigSrc string `json:"config_src"`
	}
	if err := json.Unmarshal([]byte(body), &req); err != nil {
		writeError(w, http.StatusBadRequest, "invalid request body")
		return
	}

	s.mu.Lock()
	id := s.nextID("tun", &s.nextTunnelID)
	t := Tunnel{ID: id, Name: req.Name, Secret: req.Secret, ConfigSrc: req.ConfigSrc}
	s.Tunnels[id] = t
	s.mu.Unlock()

	writeJSON(w, http.StatusOK, t)
}

func (s *Server) handleDeleteTunnel(w http.ResponseWriter, path string) {
	// path: /accounts/{accountId}/cfd_tunnel/{tunnelId}
	tunnelID := extractSegment(path, 3)

	s.mu.Lock()
	if _, ok := s.Tunnels[tunnelID]; !ok {
		s.mu.Unlock()
		writeError(w, http.StatusNotFound, "tunnel not found")
		return
	}
	delete(s.Tunnels, tunnelID)
	s.mu.Unlock()

	writeJSON(w, http.StatusOK, nil)
}

func (s *Server) handleCleanupTunnelConnections(w http.ResponseWriter, path string) {
	// path: /accounts/{accountId}/cfd_tunnel/{tunnelId}/connections
	tunnelID := extractSegment(path, 3)

	s.mu.Lock()
	_, ok := s.Tunnels[tunnelID]
	s.mu.Unlock()

	if !ok {
		writeError(w, http.StatusNotFound, "tunnel not found")
		return
	}

	writeJSON(w, http.StatusOK, nil)
}

func (s *Server) handleGetTunnel(w http.ResponseWriter, path string) {
	tunnelID := extractSegment(path, 3)

	s.mu.Lock()
	t, ok := s.Tunnels[tunnelID]
	s.mu.Unlock()

	if !ok {
		writeError(w, http.StatusNotFound, "tunnel not found")
		return
	}

	writeJSON(w, http.StatusOK, t)
}

func (s *Server) handleListTunnels(w http.ResponseWriter, r *http.Request) {
	nameFilter := r.URL.Query().Get("name")

	s.mu.Lock()
	var tunnels []Tunnel
	for _, t := range s.Tunnels {
		if nameFilter != "" && t.Name != nameFilter {
			continue
		}
		tunnels = append(tunnels, t)
	}
	s.mu.Unlock()

	writeJSONList(w, http.StatusOK, tunnels, len(tunnels))
}

func (s *Server) handleUpdateTunnelConfiguration(w http.ResponseWriter, r *http.Request, path, body string) {
	tunnelID := extractSegment(path, 3)

	s.mu.Lock()
	if _, ok := s.Tunnels[tunnelID]; !ok {
		s.mu.Unlock()
		writeError(w, http.StatusNotFound, "tunnel not found")
		return
	}
	s.TunnelConfigs[tunnelID] = TunnelConfig{
		TunnelID: tunnelID,
		Config:   json.RawMessage(body),
	}
	s.mu.Unlock()

	writeJSON(w, http.StatusOK, map[string]interface{}{
		"tunnel_id": tunnelID,
		"config":    json.RawMessage(body),
	})
}

// --- Account handlers ---

func (s *Server) handleGetAccount(w http.ResponseWriter, path string) {
	accountID := extractSegment(path, 1)

	s.mu.Lock()
	acct, ok := s.Accounts[accountID]
	s.mu.Unlock()

	if !ok {
		writeError(w, http.StatusNotFound, "account not found")
		return
	}

	writeJSON(w, http.StatusOK, acct)
}

func (s *Server) handleListAccounts(w http.ResponseWriter, r *http.Request) {
	nameFilter := r.URL.Query().Get("name")

	s.mu.Lock()
	var accounts []Account
	for _, a := range s.Accounts {
		if nameFilter != "" && a.Name != nameFilter {
			continue
		}
		accounts = append(accounts, a)
	}
	s.mu.Unlock()

	writeJSONList(w, http.StatusOK, accounts, len(accounts))
}

// --- Zone handlers ---

func (s *Server) handleListZones(w http.ResponseWriter, r *http.Request) {
	nameFilter := r.URL.Query().Get("name")

	s.mu.Lock()
	var zones []Zone
	for _, z := range s.Zones {
		if nameFilter != "" && z.Name != nameFilter {
			continue
		}
		zones = append(zones, z)
	}
	s.mu.Unlock()

	writeJSONList(w, http.StatusOK, zones, len(zones))
}

// --- DNS Record handlers ---

func (s *Server) handleListDNSRecords(w http.ResponseWriter, r *http.Request, path string) {
	zoneID := extractSegment(path, 1)
	typeFilter := r.URL.Query().Get("type")
	nameFilter := r.URL.Query().Get("name")

	s.mu.Lock()
	var records []DNSRecord
	for _, rec := range s.DNSRecords {
		if rec.ZoneID != zoneID {
			continue
		}
		if typeFilter != "" && rec.Type != typeFilter {
			continue
		}
		if nameFilter != "" && rec.Name != nameFilter {
			continue
		}
		records = append(records, rec)
	}
	s.mu.Unlock()

	writeJSONList(w, http.StatusOK, records, len(records))
}

func (s *Server) handleCreateDNSRecord(w http.ResponseWriter, path, body string) {
	zoneID := extractSegment(path, 1)

	var req struct {
		Type    string `json:"type"`
		Name    string `json:"name"`
		Content string `json:"content"`
		TTL     int    `json:"ttl"`
		Proxied *bool  `json:"proxied,omitempty"`
		Comment string `json:"comment,omitempty"`
	}
	if err := json.Unmarshal([]byte(body), &req); err != nil {
		writeError(w, http.StatusBadRequest, "invalid request body")
		return
	}

	s.mu.Lock()
	id := s.nextID("dns", &s.nextDNSID)
	comment := &req.Comment
	rec := DNSRecord{
		ID:      id,
		ZoneID:  zoneID,
		Type:    req.Type,
		Name:    req.Name,
		Content: req.Content,
		TTL:     req.TTL,
		Proxied: req.Proxied,
		Comment: comment,
	}
	s.DNSRecords[id] = rec
	s.mu.Unlock()

	writeJSON(w, http.StatusOK, rec)
}

func (s *Server) handleUpdateDNSRecord(w http.ResponseWriter, path, body string) {
	zoneID := extractSegment(path, 1)
	recordID := extractSegment(path, 3)

	s.mu.Lock()
	rec, ok := s.DNSRecords[recordID]
	if !ok || rec.ZoneID != zoneID {
		s.mu.Unlock()
		writeError(w, http.StatusNotFound, "DNS record not found")
		return
	}

	var req struct {
		Type    string `json:"type"`
		Name    string `json:"name"`
		Content string `json:"content"`
		TTL     int    `json:"ttl"`
		Proxied *bool  `json:"proxied,omitempty"`
		Comment string `json:"comment,omitempty"`
	}
	if err := json.Unmarshal([]byte(body), &req); err != nil {
		s.mu.Unlock()
		writeError(w, http.StatusBadRequest, "invalid request body")
		return
	}

	comment := &req.Comment
	rec.Type = req.Type
	rec.Name = req.Name
	rec.Content = req.Content
	rec.TTL = req.TTL
	rec.Proxied = req.Proxied
	rec.Comment = comment
	s.DNSRecords[recordID] = rec
	s.mu.Unlock()

	writeJSON(w, http.StatusOK, rec)
}

func (s *Server) handleDeleteDNSRecord(w http.ResponseWriter, path string) {
	zoneID := extractSegment(path, 1)
	recordID := extractSegment(path, 3)

	s.mu.Lock()
	rec, ok := s.DNSRecords[recordID]
	if !ok || rec.ZoneID != zoneID {
		s.mu.Unlock()
		writeError(w, http.StatusNotFound, "DNS record not found")
		return
	}
	delete(s.DNSRecords, recordID)
	s.mu.Unlock()

	writeJSON(w, http.StatusOK, map[string]string{"id": recordID})
}
