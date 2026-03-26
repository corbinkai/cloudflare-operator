package controller

import (
	"context"
	"testing"

	networkingv1alpha1 "github.com/adyanth/cloudflare-operator/api/v1alpha1"
	networkingv1alpha2 "github.com/adyanth/cloudflare-operator/api/v1alpha2"
	corev1 "k8s.io/api/core/v1"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/apimachinery/pkg/runtime"
	"sigs.k8s.io/controller-runtime/pkg/client/fake"
	"sigs.k8s.io/controller-runtime/pkg/log/zap"
)

func baseTunnelSpec() networkingv1alpha2.TunnelSpec {
	return networkingv1alpha2.TunnelSpec{
		Cloudflare: networkingv1alpha2.CloudflareDetails{
			Domain:                            "example.com",
			Secret:                            "cf-secret",
			AccountName:                       "my-account",
			AccountId:                         "acc-id-1",
			CLOUDFLARE_API_TOKEN:              "CLOUDFLARE_API_TOKEN",
			CLOUDFLARE_API_KEY:                "CLOUDFLARE_API_KEY",
			CLOUDFLARE_TUNNEL_CREDENTIAL_FILE: "CLOUDFLARE_TUNNEL_CREDENTIAL_FILE",
		},
		FallbackTarget: "http_status:404",
	}
}

func baseTunnelStatus() networkingv1alpha2.TunnelStatus {
	return networkingv1alpha2.TunnelStatus{
		TunnelId:   "tid-100",
		TunnelName: "prod-tunnel",
		AccountId:  "aid-100",
		ZoneId:     "zid-100",
	}
}

func newUtilsScheme() *runtime.Scheme {
	s := runtime.NewScheme()
	_ = corev1.AddToScheme(s)
	return s
}

func TestGetAPIDetails_WithAPIToken(t *testing.T) {
	secret := &corev1.Secret{
		ObjectMeta: metav1.ObjectMeta{
			Name:      "cf-secret",
			Namespace: "default",
		},
		Data: map[string][]byte{
			"CLOUDFLARE_API_TOKEN": []byte("my-token-value"),
		},
	}
	c := fake.NewClientBuilder().WithScheme(newUtilsScheme()).WithObjects(secret).Build()
	log := zap.New(zap.UseDevMode(true))

	cfAPI, returnedSecret, err := getAPIDetails(context.Background(), c, log, baseTunnelSpec(), baseTunnelStatus(), "default", nil)
	if err != nil {
		t.Fatalf("expected no error, got: %v", err)
	}
	if returnedSecret.Name != "cf-secret" {
		t.Errorf("returned secret name = %q, want %q", returnedSecret.Name, "cf-secret")
	}
	if cfAPI.CloudflareClient == nil {
		t.Errorf("expected CloudflareClient to be non-nil")
	}
	if cfAPI.Domain != "example.com" {
		t.Errorf("cfAPI.Domain = %q, want %q", cfAPI.Domain, "example.com")
	}
}

func TestGetAPIDetails_WithAPIKey(t *testing.T) {
	spec := baseTunnelSpec()
	spec.Cloudflare.Email = "user@example.com"

	secret := &corev1.Secret{
		ObjectMeta: metav1.ObjectMeta{
			Name:      "cf-secret",
			Namespace: "default",
		},
		Data: map[string][]byte{
			"CLOUDFLARE_API_KEY": []byte("my-api-key"),
		},
	}
	c := fake.NewClientBuilder().WithScheme(newUtilsScheme()).WithObjects(secret).Build()
	log := zap.New(zap.UseDevMode(true))

	cfAPI, _, err := getAPIDetails(context.Background(), c, log, spec, baseTunnelStatus(), "default", nil)
	if err != nil {
		t.Fatalf("expected no error, got: %v", err)
	}
	if cfAPI.CloudflareClient == nil {
		t.Errorf("expected CloudflareClient to be non-nil")
	}
}

func TestGetAPIDetails_SecretNotFound(t *testing.T) {
	c := fake.NewClientBuilder().WithScheme(newUtilsScheme()).Build()
	log := zap.New(zap.UseDevMode(true))

	_, _, err := getAPIDetails(context.Background(), c, log, baseTunnelSpec(), baseTunnelStatus(), "default", nil)
	if err == nil {
		t.Fatal("expected error for missing secret, got nil")
	}
}

func TestGetAPIDetails_NoTokenOrKey(t *testing.T) {
	secret := &corev1.Secret{
		ObjectMeta: metav1.ObjectMeta{
			Name:      "cf-secret",
			Namespace: "default",
		},
		Data: map[string][]byte{
			"SOME_OTHER_KEY": []byte("irrelevant"),
		},
	}
	c := fake.NewClientBuilder().WithScheme(newUtilsScheme()).WithObjects(secret).Build()
	log := zap.New(zap.UseDevMode(true))

	_, _, err := getAPIDetails(context.Background(), c, log, baseTunnelSpec(), baseTunnelStatus(), "default", nil)
	if err == nil {
		t.Fatal("expected error when secret has no recognized keys, got nil")
	}
}

func TestGetAPIDetails_WithSecretOverride(t *testing.T) {
	// The tunnel spec points to "cf-secret" in "default", but the override points elsewhere
	overrideSecret := &corev1.Secret{
		ObjectMeta: metav1.ObjectMeta{
			Name:      "override-secret",
			Namespace: "other-ns",
		},
		Data: map[string][]byte{
			"CLOUDFLARE_API_TOKEN": []byte("override-token"),
		},
	}
	c := fake.NewClientBuilder().WithScheme(newUtilsScheme()).WithObjects(overrideSecret).Build()
	log := zap.New(zap.UseDevMode(true))

	override := &networkingv1alpha1.SecretReference{
		Name:      "override-secret",
		Namespace: "other-ns",
	}

	cfAPI, returnedSecret, err := getAPIDetails(context.Background(), c, log, baseTunnelSpec(), baseTunnelStatus(), "default", override)
	if err != nil {
		t.Fatalf("expected no error, got: %v", err)
	}
	if returnedSecret.Name != "override-secret" {
		t.Errorf("returned secret name = %q, want %q", returnedSecret.Name, "override-secret")
	}
	if returnedSecret.Namespace != "other-ns" {
		t.Errorf("returned secret namespace = %q, want %q", returnedSecret.Namespace, "other-ns")
	}
	if cfAPI.CloudflareClient == nil {
		t.Errorf("expected CloudflareClient to be non-nil")
	}
}

func TestGetAPIDetails_WithoutSecretOverride(t *testing.T) {
	secret := &corev1.Secret{
		ObjectMeta: metav1.ObjectMeta{
			Name:      "cf-secret",
			Namespace: "my-ns",
		},
		Data: map[string][]byte{
			"CLOUDFLARE_API_TOKEN": []byte("token-val"),
		},
	}
	c := fake.NewClientBuilder().WithScheme(newUtilsScheme()).WithObjects(secret).Build()
	log := zap.New(zap.UseDevMode(true))

	cfAPI, returnedSecret, err := getAPIDetails(context.Background(), c, log, baseTunnelSpec(), baseTunnelStatus(), "my-ns", nil)
	if err != nil {
		t.Fatalf("expected no error, got: %v", err)
	}
	if returnedSecret.Name != "cf-secret" {
		t.Errorf("returned secret name = %q, want %q", returnedSecret.Name, "cf-secret")
	}
	if returnedSecret.Namespace != "my-ns" {
		t.Errorf("returned secret namespace = %q, want %q", returnedSecret.Namespace, "my-ns")
	}
	if cfAPI.Domain != "example.com" {
		t.Errorf("cfAPI.Domain = %q, want %q", cfAPI.Domain, "example.com")
	}
}

func TestGetAPIDetails_PopulatesAPIFields(t *testing.T) {
	secret := &corev1.Secret{
		ObjectMeta: metav1.ObjectMeta{
			Name:      "cf-secret",
			Namespace: "default",
		},
		Data: map[string][]byte{
			"CLOUDFLARE_API_TOKEN": []byte("tok"),
		},
	}
	c := fake.NewClientBuilder().WithScheme(newUtilsScheme()).WithObjects(secret).Build()
	log := zap.New(zap.UseDevMode(true))

	spec := baseTunnelSpec()
	status := baseTunnelStatus()

	cfAPI, _, err := getAPIDetails(context.Background(), c, log, spec, status, "default", nil)
	if err != nil {
		t.Fatalf("expected no error, got: %v", err)
	}

	if cfAPI.AccountName != "my-account" {
		t.Errorf("cfAPI.AccountName = %q, want %q", cfAPI.AccountName, "my-account")
	}
	if cfAPI.AccountId != "acc-id-1" {
		t.Errorf("cfAPI.AccountId = %q, want %q", cfAPI.AccountId, "acc-id-1")
	}
	if cfAPI.Domain != "example.com" {
		t.Errorf("cfAPI.Domain = %q, want %q", cfAPI.Domain, "example.com")
	}
	if cfAPI.ValidAccountId != "aid-100" {
		t.Errorf("cfAPI.ValidAccountId = %q, want %q", cfAPI.ValidAccountId, "aid-100")
	}
	if cfAPI.ValidTunnelId != "tid-100" {
		t.Errorf("cfAPI.ValidTunnelId = %q, want %q", cfAPI.ValidTunnelId, "tid-100")
	}
	if cfAPI.ValidTunnelName != "prod-tunnel" {
		t.Errorf("cfAPI.ValidTunnelName = %q, want %q", cfAPI.ValidTunnelName, "prod-tunnel")
	}
	if cfAPI.ValidZoneId != "zid-100" {
		t.Errorf("cfAPI.ValidZoneId = %q, want %q", cfAPI.ValidZoneId, "zid-100")
	}
}

func TestGetCloudflareClient_Token(t *testing.T) {
	client, err := getCloudflareClient("", "", "my-api-token")
	if err != nil {
		t.Fatalf("expected no error, got: %v", err)
	}
	if client == nil {
		t.Error("expected non-nil cloudflare client")
	}
}

func TestGetCloudflareClient_APIKey(t *testing.T) {
	client, err := getCloudflareClient("my-api-key", "user@example.com", "")
	if err != nil {
		t.Fatalf("expected no error, got: %v", err)
	}
	if client == nil {
		t.Error("expected non-nil cloudflare client")
	}
}
