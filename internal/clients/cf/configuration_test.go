package cf

import (
	"testing"
	"time"

	"gopkg.in/yaml.v3"
	"k8s.io/utils/ptr"
)

func TestConfiguration_YAMLRoundTrip(t *testing.T) {
	original := Configuration{
		TunnelId: "abc-123",
		Ingress: []UnvalidatedIngressRule{
			{
				Hostname: "app.example.com",
				Service:  "http://localhost:8080",
				Path:     "/api",
				OriginRequest: OriginRequestConfig{
					NoTLSVerify:    ptr.To(true),
					HTTPHostHeader: ptr.To("custom.host"),
				},
			},
			{
				Service: "http_status:404",
			},
		},
		WarpRouting: WarpRoutingConfig{
			Enabled: true,
		},
		OriginRequest: OriginRequestConfig{
			NoHappyEyeballs:      ptr.To(false),
			KeepAliveConnections: ptr.To(100),
		},
		SourceFile:   "/etc/cloudflared/credentials.json",
		Metrics:      "localhost:2000",
		NoAutoUpdate: true,
	}

	data, err := yaml.Marshal(&original)
	if err != nil {
		t.Fatalf("failed to marshal: %v", err)
	}

	var decoded Configuration
	if err := yaml.Unmarshal(data, &decoded); err != nil {
		t.Fatalf("failed to unmarshal: %v", err)
	}

	if decoded.TunnelId != original.TunnelId {
		t.Errorf("TunnelId: expected %s, got %s", original.TunnelId, decoded.TunnelId)
	}
	if len(decoded.Ingress) != 2 {
		t.Fatalf("Ingress: expected 2 rules, got %d", len(decoded.Ingress))
	}
	if decoded.Ingress[0].Hostname != "app.example.com" {
		t.Errorf("Ingress[0].Hostname: expected app.example.com, got %s", decoded.Ingress[0].Hostname)
	}
	if decoded.Ingress[0].Service != "http://localhost:8080" {
		t.Errorf("Ingress[0].Service: expected http://localhost:8080, got %s", decoded.Ingress[0].Service)
	}
	if decoded.Ingress[0].Path != "/api" {
		t.Errorf("Ingress[0].Path: expected /api, got %s", decoded.Ingress[0].Path)
	}
	if decoded.Ingress[0].OriginRequest.NoTLSVerify == nil || *decoded.Ingress[0].OriginRequest.NoTLSVerify != true {
		t.Error("Ingress[0].OriginRequest.NoTLSVerify mismatch")
	}
	if decoded.Ingress[0].OriginRequest.HTTPHostHeader == nil || *decoded.Ingress[0].OriginRequest.HTTPHostHeader != "custom.host" {
		t.Error("Ingress[0].OriginRequest.HTTPHostHeader mismatch")
	}
	if decoded.Ingress[1].Service != "http_status:404" {
		t.Errorf("Ingress[1].Service: expected http_status:404, got %s", decoded.Ingress[1].Service)
	}
	if decoded.Ingress[1].Hostname != "" {
		t.Errorf("Ingress[1].Hostname should be empty, got %s", decoded.Ingress[1].Hostname)
	}
	if !decoded.WarpRouting.Enabled {
		t.Error("WarpRouting.Enabled should be true")
	}
	if decoded.SourceFile != original.SourceFile {
		t.Errorf("SourceFile: expected %s, got %s", original.SourceFile, decoded.SourceFile)
	}
	if decoded.Metrics != "localhost:2000" {
		t.Errorf("Metrics: expected localhost:2000, got %s", decoded.Metrics)
	}
	if !decoded.NoAutoUpdate {
		t.Error("NoAutoUpdate should be true")
	}
}

func TestUnvalidatedIngressRule_YAMLRoundTrip(t *testing.T) {
	tests := []struct {
		name string
		rule UnvalidatedIngressRule
	}{
		{
			name: "full rule",
			rule: UnvalidatedIngressRule{
				Hostname: "api.example.com",
				Service:  "https://api-backend:443",
				Path:     "/v2/.*",
				OriginRequest: OriginRequestConfig{
					NoTLSVerify:    ptr.To(false),
					Http2Origin:    ptr.To(true),
					BastionMode:    ptr.To(false),
					ProxyAddress:   ptr.To("0.0.0.0"),
					ProxyPort:      ptr.To(uint(8080)),
					ProxyType:      ptr.To("socks"),
				},
			},
		},
		{
			name: "catch-all",
			rule: UnvalidatedIngressRule{
				Service: "http_status:404",
			},
		},
		{
			name: "hostname only",
			rule: UnvalidatedIngressRule{
				Hostname: "web.example.com",
				Service:  "http://web:80",
			},
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			data, err := yaml.Marshal(&tc.rule)
			if err != nil {
				t.Fatalf("failed to marshal: %v", err)
			}

			var decoded UnvalidatedIngressRule
			if err := yaml.Unmarshal(data, &decoded); err != nil {
				t.Fatalf("failed to unmarshal: %v", err)
			}

			if decoded.Hostname != tc.rule.Hostname {
				t.Errorf("Hostname: expected %q, got %q", tc.rule.Hostname, decoded.Hostname)
			}
			if decoded.Service != tc.rule.Service {
				t.Errorf("Service: expected %q, got %q", tc.rule.Service, decoded.Service)
			}
			if decoded.Path != tc.rule.Path {
				t.Errorf("Path: expected %q, got %q", tc.rule.Path, decoded.Path)
			}
		})
	}
}

func TestOriginRequestConfig_YAMLRoundTrip(t *testing.T) {
	connectTimeout := 30 * time.Second
	tlsTimeout := 10 * time.Second
	tcpKeepAlive := 30 * time.Second
	keepAliveTimeout := 90 * time.Second

	original := OriginRequestConfig{
		ConnectTimeout:         &connectTimeout,
		TLSTimeout:             &tlsTimeout,
		TCPKeepAlive:           &tcpKeepAlive,
		NoHappyEyeballs:        ptr.To(true),
		KeepAliveConnections:   ptr.To(50),
		KeepAliveTimeout:       &keepAliveTimeout,
		HTTPHostHeader:         ptr.To("backend.internal"),
		OriginServerName:       ptr.To("backend.example.com"),
		CAPool:                 ptr.To("/etc/ssl/ca.pem"),
		NoTLSVerify:            ptr.To(false),
		Http2Origin:            ptr.To(true),
		DisableChunkedEncoding: ptr.To(false),
		BastionMode:            ptr.To(true),
		ProxyAddress:           ptr.To("192.168.1.1"),
		ProxyPort:              ptr.To(uint(3128)),
		ProxyType:              ptr.To(""),
		IPRules: []IngressIPRule{
			{
				Prefix: ptr.To("192.168.0.0/16"),
				Ports:  []int{80, 443},
				Allow:  true,
			},
			{
				Prefix: ptr.To("10.0.0.0/8"),
				Ports:  []int{22},
				Allow:  false,
			},
		},
	}

	data, err := yaml.Marshal(&original)
	if err != nil {
		t.Fatalf("failed to marshal: %v", err)
	}

	var decoded OriginRequestConfig
	if err := yaml.Unmarshal(data, &decoded); err != nil {
		t.Fatalf("failed to unmarshal: %v", err)
	}

	// Duration fields
	if decoded.ConnectTimeout == nil || *decoded.ConnectTimeout != 30*time.Second {
		t.Error("ConnectTimeout mismatch")
	}
	if decoded.TLSTimeout == nil || *decoded.TLSTimeout != 10*time.Second {
		t.Error("TLSTimeout mismatch")
	}
	if decoded.TCPKeepAlive == nil || *decoded.TCPKeepAlive != 30*time.Second {
		t.Error("TCPKeepAlive mismatch")
	}
	if decoded.KeepAliveTimeout == nil || *decoded.KeepAliveTimeout != 90*time.Second {
		t.Error("KeepAliveTimeout mismatch")
	}

	// Bool fields
	if decoded.NoHappyEyeballs == nil || *decoded.NoHappyEyeballs != true {
		t.Error("NoHappyEyeballs mismatch")
	}
	if decoded.NoTLSVerify == nil || *decoded.NoTLSVerify != false {
		t.Error("NoTLSVerify mismatch")
	}
	if decoded.Http2Origin == nil || *decoded.Http2Origin != true {
		t.Error("Http2Origin mismatch")
	}
	if decoded.DisableChunkedEncoding == nil || *decoded.DisableChunkedEncoding != false {
		t.Error("DisableChunkedEncoding mismatch")
	}
	if decoded.BastionMode == nil || *decoded.BastionMode != true {
		t.Error("BastionMode mismatch")
	}

	// Int/string fields
	if decoded.KeepAliveConnections == nil || *decoded.KeepAliveConnections != 50 {
		t.Error("KeepAliveConnections mismatch")
	}
	if decoded.HTTPHostHeader == nil || *decoded.HTTPHostHeader != "backend.internal" {
		t.Error("HTTPHostHeader mismatch")
	}
	if decoded.OriginServerName == nil || *decoded.OriginServerName != "backend.example.com" {
		t.Error("OriginServerName mismatch")
	}
	if decoded.CAPool == nil || *decoded.CAPool != "/etc/ssl/ca.pem" {
		t.Error("CAPool mismatch")
	}
	if decoded.ProxyAddress == nil || *decoded.ProxyAddress != "192.168.1.1" {
		t.Error("ProxyAddress mismatch")
	}
	if decoded.ProxyPort == nil || *decoded.ProxyPort != 3128 {
		t.Error("ProxyPort mismatch")
	}
	if decoded.ProxyType == nil || *decoded.ProxyType != "" {
		t.Error("ProxyType mismatch")
	}

	// IP rules
	if len(decoded.IPRules) != 2 {
		t.Fatalf("IPRules: expected 2, got %d", len(decoded.IPRules))
	}
	if decoded.IPRules[0].Prefix == nil || *decoded.IPRules[0].Prefix != "192.168.0.0/16" {
		t.Error("IPRules[0].Prefix mismatch")
	}
	if len(decoded.IPRules[0].Ports) != 2 || decoded.IPRules[0].Ports[0] != 80 || decoded.IPRules[0].Ports[1] != 443 {
		t.Error("IPRules[0].Ports mismatch")
	}
	if !decoded.IPRules[0].Allow {
		t.Error("IPRules[0].Allow should be true")
	}
	if decoded.IPRules[1].Prefix == nil || *decoded.IPRules[1].Prefix != "10.0.0.0/8" {
		t.Error("IPRules[1].Prefix mismatch")
	}
	if decoded.IPRules[1].Allow {
		t.Error("IPRules[1].Allow should be false")
	}
}

func TestOriginRequestConfig_EmptyYAML(t *testing.T) {
	var config OriginRequestConfig
	data, err := yaml.Marshal(&config)
	if err != nil {
		t.Fatalf("failed to marshal empty config: %v", err)
	}

	var decoded OriginRequestConfig
	if err := yaml.Unmarshal(data, &decoded); err != nil {
		t.Fatalf("failed to unmarshal: %v", err)
	}

	if decoded.ConnectTimeout != nil {
		t.Error("expected nil ConnectTimeout")
	}
	if decoded.NoTLSVerify != nil {
		t.Error("expected nil NoTLSVerify")
	}
	if decoded.KeepAliveConnections != nil {
		t.Error("expected nil KeepAliveConnections")
	}
	if decoded.HTTPHostHeader != nil {
		t.Error("expected nil HTTPHostHeader")
	}
	if len(decoded.IPRules) != 0 {
		t.Error("expected empty IPRules")
	}
}

func TestConfiguration_MinimalYAML(t *testing.T) {
	yamlStr := `
tunnel: my-tunnel-id
credentials-file: /etc/cloudflared/creds.json
ingress:
  - service: http_status:404
`
	var config Configuration
	if err := yaml.Unmarshal([]byte(yamlStr), &config); err != nil {
		t.Fatalf("failed to unmarshal: %v", err)
	}

	if config.TunnelId != "my-tunnel-id" {
		t.Errorf("TunnelId: expected my-tunnel-id, got %s", config.TunnelId)
	}
	if config.SourceFile != "/etc/cloudflared/creds.json" {
		t.Errorf("SourceFile: expected /etc/cloudflared/creds.json, got %s", config.SourceFile)
	}
	if len(config.Ingress) != 1 {
		t.Fatalf("Ingress: expected 1, got %d", len(config.Ingress))
	}
	if config.Ingress[0].Service != "http_status:404" {
		t.Errorf("Ingress[0].Service: expected http_status:404, got %s", config.Ingress[0].Service)
	}
}
