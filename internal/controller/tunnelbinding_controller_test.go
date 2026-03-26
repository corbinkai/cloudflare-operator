package controller

import (
	"context"
	"testing"

	networkingv1alpha1 "github.com/adyanth/cloudflare-operator/api/v1alpha1"
	networkingv1alpha2 "github.com/adyanth/cloudflare-operator/api/v1alpha2"
	"github.com/adyanth/cloudflare-operator/internal/clients/cf"
	corev1 "k8s.io/api/core/v1"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/apimachinery/pkg/runtime"
	apitypes "k8s.io/apimachinery/pkg/types"
	"k8s.io/client-go/tools/record"
	"sigs.k8s.io/controller-runtime/pkg/client/fake"
	"sigs.k8s.io/controller-runtime/pkg/reconcile"
)

func newScheme(t *testing.T) *runtime.Scheme {
	t.Helper()
	s := runtime.NewScheme()
	if err := networkingv1alpha1.AddToScheme(s); err != nil {
		t.Fatal(err)
	}
	if err := networkingv1alpha2.AddToScheme(s); err != nil {
		t.Fatal(err)
	}
	if err := corev1.AddToScheme(s); err != nil {
		t.Fatal(err)
	}
	return s
}

func TestLabelsForBinding(t *testing.T) {
	binding := networkingv1alpha1.TunnelBinding{
		TunnelRef: networkingv1alpha1.TunnelRef{
			Kind: "Tunnel",
			Name: "my-tunnel",
		},
	}
	labels := labelsForBinding(binding)

	if labels[tunnelNameLabel] != "my-tunnel" {
		t.Errorf("expected tunnelNameLabel=%q, got %q", "my-tunnel", labels[tunnelNameLabel])
	}
	if labels[tunnelKindLabel] != "Tunnel" {
		t.Errorf("expected tunnelKindLabel=%q, got %q", "Tunnel", labels[tunnelKindLabel])
	}
}

func TestLabelsForBinding_ClusterTunnel(t *testing.T) {
	binding := networkingv1alpha1.TunnelBinding{
		TunnelRef: networkingv1alpha1.TunnelRef{
			Kind: "ClusterTunnel",
			Name: "ct-1",
		},
	}
	labels := labelsForBinding(binding)

	if labels[tunnelKindLabel] != "ClusterTunnel" {
		t.Errorf("expected tunnelKindLabel=%q, got %q", "ClusterTunnel", labels[tunnelKindLabel])
	}
	if labels[tunnelNameLabel] != "ct-1" {
		t.Errorf("expected tunnelNameLabel=%q, got %q", "ct-1", labels[tunnelNameLabel])
	}
}

func TestGetServiceProto(t *testing.T) {
	r := &TunnelBindingReconciler{}

	tests := []struct {
		name         string
		tunnelProto  string
		validProto   bool
		portProto    corev1.Protocol
		port         int32
		wantProto    string
	}{
		{"TCP port 80 -> http", "", false, corev1.ProtocolTCP, 80, tunnelProtoHTTP},
		{"TCP port 443 -> https", "", false, corev1.ProtocolTCP, 443, tunnelProtoHTTPS},
		{"TCP port 22 -> ssh", "", false, corev1.ProtocolTCP, 22, tunnelProtoSSH},
		{"TCP port 3389 -> rdp", "", false, corev1.ProtocolTCP, 3389, tunnelProtoRDP},
		{"TCP port 139 -> smb", "", false, corev1.ProtocolTCP, 139, tunnelProtoSMB},
		{"TCP port 445 -> smb", "", false, corev1.ProtocolTCP, 445, tunnelProtoSMB},
		{"TCP port 8080 -> http (default)", "", false, corev1.ProtocolTCP, 8080, tunnelProtoHTTP},
		{"UDP -> udp", "", false, corev1.ProtocolUDP, 5000, tunnelProtoUDP},
		{"Explicit https on any port", "https", true, corev1.ProtocolTCP, 8080, tunnelProtoHTTPS},
		{"Explicit tcp", "tcp", true, corev1.ProtocolTCP, 80, tunnelProtoTCP},
		{"Invalid protocol falls back to default", "invalid", false, corev1.ProtocolTCP, 80, tunnelProtoHTTP},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			port := corev1.ServicePort{
				Protocol: tt.portProto,
				Port:     tt.port,
			}
			got := r.getServiceProto(tt.tunnelProto, tt.validProto, port)
			if got != tt.wantProto {
				t.Errorf("getServiceProto(%q, %v, port{%s,%d}) = %q, want %q",
					tt.tunnelProto, tt.validProto, tt.portProto, tt.port, got, tt.wantProto)
			}
		})
	}
}

func TestReconcile_NotFound_NoPanic(t *testing.T) {
	s := newScheme(t)
	fakeClient := fake.NewClientBuilder().WithScheme(s).Build()

	r := &TunnelBindingReconciler{
		Client:   fakeClient,
		Scheme:   s,
		Recorder: record.NewFakeRecorder(10),
	}

	result, err := r.Reconcile(context.Background(), reconcile.Request{
		NamespacedName: apitypes.NamespacedName{
			Name:      "nonexistent-binding",
			Namespace: "default",
		},
	})

	if err != nil {
		t.Errorf("expected nil error, got %v", err)
	}
	if result.Requeue || result.RequeueAfter != 0 {
		t.Errorf("expected empty Result, got %+v", result)
	}
}

func TestReconcile_ClearsStaleState(t *testing.T) {
	s := newScheme(t)
	fakeClient := fake.NewClientBuilder().WithScheme(s).Build()

	r := &TunnelBindingReconciler{
		Client:   fakeClient,
		Scheme:   s,
		Recorder: record.NewFakeRecorder(10),
	}

	// Set stale state from a hypothetical previous reconcile
	r.binding = &networkingv1alpha1.TunnelBinding{
		ObjectMeta: metav1.ObjectMeta{Name: "stale", Namespace: "default"},
	}
	r.configmap = &corev1.ConfigMap{
		ObjectMeta: metav1.ObjectMeta{Name: "stale-cm", Namespace: "default"},
	}
	r.cfAPI = &cf.API{TunnelName: "stale-tunnel"}
	r.fallbackTarget = "http_status:503"

	_, _ = r.Reconcile(context.Background(), reconcile.Request{
		NamespacedName: apitypes.NamespacedName{
			Name:      "nonexistent",
			Namespace: "default",
		},
	})

	if r.binding != nil {
		t.Errorf("expected r.binding to be nil after reconcile, got %+v", r.binding)
	}
	if r.configmap != nil {
		t.Errorf("expected r.configmap to be nil after reconcile, got %+v", r.configmap)
	}
	if r.cfAPI != nil {
		t.Errorf("expected r.cfAPI to be nil after reconcile, got %+v", r.cfAPI)
	}
	if r.fallbackTarget != "" {
		t.Errorf("expected r.fallbackTarget to be empty, got %q", r.fallbackTarget)
	}
}
