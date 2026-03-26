package controller

import (
	"testing"

	networkingv1alpha2 "github.com/adyanth/cloudflare-operator/api/v1alpha2"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/apimachinery/pkg/types"
)

func newTestTunnel() *networkingv1alpha2.Tunnel {
	return &networkingv1alpha2.Tunnel{
		ObjectMeta: metav1.ObjectMeta{
			Name:      "test-tunnel",
			Namespace: "test-ns",
			UID:       types.UID("abc-123"),
			Labels:    map[string]string{"env": "test"},
			Annotations: map[string]string{
				"note": "hello",
			},
		},
		Spec: networkingv1alpha2.TunnelSpec{
			Cloudflare: networkingv1alpha2.CloudflareDetails{
				Domain:      "example.com",
				AccountName: "acct",
			},
			FallbackTarget: "http_status:404",
		},
		Status: networkingv1alpha2.TunnelStatus{
			TunnelId:   "tid-1",
			TunnelName: "my-tunnel",
			AccountId:  "aid-1",
			ZoneId:     "zid-1",
		},
	}
}

func newTestClusterTunnel() *networkingv1alpha2.ClusterTunnel {
	return &networkingv1alpha2.ClusterTunnel{
		ObjectMeta: metav1.ObjectMeta{
			Name: "cluster-tunnel",
			UID:  types.UID("def-456"),
			Labels: map[string]string{
				"scope": "cluster",
			},
			Annotations: map[string]string{
				"desc": "cluster-wide",
			},
		},
		Spec: networkingv1alpha2.TunnelSpec{
			Cloudflare: networkingv1alpha2.CloudflareDetails{
				Domain:      "cluster.example.com",
				AccountName: "cluster-acct",
			},
			FallbackTarget: "http_status:503",
		},
		Status: networkingv1alpha2.TunnelStatus{
			TunnelId:   "ctid-1",
			TunnelName: "cluster-my-tunnel",
			AccountId:  "caid-1",
			ZoneId:     "czid-1",
		},
	}
}

func TestTunnelAdapter_GetObject(t *testing.T) {
	tunnel := newTestTunnel()
	adapter := TunnelAdapter{Tunnel: tunnel}

	obj := adapter.GetObject()
	if obj != tunnel {
		t.Errorf("GetObject() should return the underlying *Tunnel pointer")
	}
}

func TestTunnelAdapter_GetNamespace(t *testing.T) {
	adapter := TunnelAdapter{Tunnel: newTestTunnel()}

	if got := adapter.GetNamespace(); got != "test-ns" {
		t.Errorf("GetNamespace() = %q, want %q", got, "test-ns")
	}
}

func TestTunnelAdapter_GetName(t *testing.T) {
	adapter := TunnelAdapter{Tunnel: newTestTunnel()}

	if got := adapter.GetName(); got != "test-tunnel" {
		t.Errorf("GetName() = %q, want %q", got, "test-tunnel")
	}
}

func TestTunnelAdapter_GetUID(t *testing.T) {
	adapter := TunnelAdapter{Tunnel: newTestTunnel()}

	if got := adapter.GetUID(); got != types.UID("abc-123") {
		t.Errorf("GetUID() = %q, want %q", got, "abc-123")
	}
}

func TestTunnelAdapter_GetLabels_SetLabels(t *testing.T) {
	adapter := TunnelAdapter{Tunnel: newTestTunnel()}

	labels := adapter.GetLabels()
	if labels["env"] != "test" {
		t.Errorf("GetLabels()[\"env\"] = %q, want %q", labels["env"], "test")
	}

	newLabels := map[string]string{"env": "prod", "tier": "frontend"}
	adapter.SetLabels(newLabels)

	got := adapter.GetLabels()
	if got["env"] != "prod" {
		t.Errorf("after SetLabels, GetLabels()[\"env\"] = %q, want %q", got["env"], "prod")
	}
	if got["tier"] != "frontend" {
		t.Errorf("after SetLabels, GetLabels()[\"tier\"] = %q, want %q", got["tier"], "frontend")
	}
}

func TestTunnelAdapter_GetAnnotations_SetAnnotations(t *testing.T) {
	adapter := TunnelAdapter{Tunnel: newTestTunnel()}

	annotations := adapter.GetAnnotations()
	if annotations["note"] != "hello" {
		t.Errorf("GetAnnotations()[\"note\"] = %q, want %q", annotations["note"], "hello")
	}

	newAnnotations := map[string]string{"note": "updated", "extra": "val"}
	adapter.SetAnnotations(newAnnotations)

	got := adapter.GetAnnotations()
	if got["note"] != "updated" {
		t.Errorf("after SetAnnotations, GetAnnotations()[\"note\"] = %q, want %q", got["note"], "updated")
	}
	if got["extra"] != "val" {
		t.Errorf("after SetAnnotations, GetAnnotations()[\"extra\"] = %q, want %q", got["extra"], "val")
	}
}

func TestTunnelAdapter_GetSpec(t *testing.T) {
	tunnel := newTestTunnel()
	adapter := TunnelAdapter{Tunnel: tunnel}

	spec := adapter.GetSpec()
	if spec.Cloudflare.Domain != "example.com" {
		t.Errorf("GetSpec().Cloudflare.Domain = %q, want %q", spec.Cloudflare.Domain, "example.com")
	}
	if spec.FallbackTarget != "http_status:404" {
		t.Errorf("GetSpec().FallbackTarget = %q, want %q", spec.FallbackTarget, "http_status:404")
	}
}

func TestTunnelAdapter_GetStatus_SetStatus(t *testing.T) {
	adapter := TunnelAdapter{Tunnel: newTestTunnel()}

	status := adapter.GetStatus()
	if status.TunnelId != "tid-1" {
		t.Errorf("GetStatus().TunnelId = %q, want %q", status.TunnelId, "tid-1")
	}

	newStatus := networkingv1alpha2.TunnelStatus{
		TunnelId:   "tid-2",
		TunnelName: "updated-tunnel",
		AccountId:  "aid-2",
		ZoneId:     "zid-2",
	}
	adapter.SetStatus(newStatus)

	got := adapter.GetStatus()
	if got.TunnelId != "tid-2" {
		t.Errorf("after SetStatus, GetStatus().TunnelId = %q, want %q", got.TunnelId, "tid-2")
	}
	if got.TunnelName != "updated-tunnel" {
		t.Errorf("after SetStatus, GetStatus().TunnelName = %q, want %q", got.TunnelName, "updated-tunnel")
	}
	if got.AccountId != "aid-2" {
		t.Errorf("after SetStatus, GetStatus().AccountId = %q, want %q", got.AccountId, "aid-2")
	}
	if got.ZoneId != "zid-2" {
		t.Errorf("after SetStatus, GetStatus().ZoneId = %q, want %q", got.ZoneId, "zid-2")
	}
}

func TestTunnelAdapter_DeepCopyTunnel(t *testing.T) {
	adapter := TunnelAdapter{Tunnel: newTestTunnel()}

	copied := adapter.DeepCopyTunnel()

	if copied.GetName() != adapter.GetName() {
		t.Errorf("DeepCopyTunnel().GetName() = %q, want %q", copied.GetName(), adapter.GetName())
	}
	if copied.GetNamespace() != adapter.GetNamespace() {
		t.Errorf("DeepCopyTunnel().GetNamespace() = %q, want %q", copied.GetNamespace(), adapter.GetNamespace())
	}

	// Mutate the copy and verify the original is unchanged
	copied.SetStatus(networkingv1alpha2.TunnelStatus{TunnelId: "mutated"})
	if adapter.GetStatus().TunnelId != "tid-1" {
		t.Errorf("original status mutated after DeepCopyTunnel; got TunnelId = %q, want %q", adapter.GetStatus().TunnelId, "tid-1")
	}

	copied.SetLabels(map[string]string{"mutated": "true"})
	if _, ok := adapter.GetLabels()["mutated"]; ok {
		t.Errorf("original labels mutated after DeepCopyTunnel")
	}
}

// ClusterTunnelAdapter tests

func TestClusterTunnelAdapter_GetObject(t *testing.T) {
	ct := newTestClusterTunnel()
	adapter := ClusterTunnelAdapter{Tunnel: ct, Namespace: "operator-ns"}

	obj := adapter.GetObject()
	if obj != ct {
		t.Errorf("GetObject() should return the underlying *ClusterTunnel pointer")
	}
}

func TestClusterTunnelAdapter_GetNamespace(t *testing.T) {
	adapter := ClusterTunnelAdapter{Tunnel: newTestClusterTunnel(), Namespace: "operator-ns"}

	if got := adapter.GetNamespace(); got != "operator-ns" {
		t.Errorf("GetNamespace() = %q, want %q", got, "operator-ns")
	}
}

func TestClusterTunnelAdapter_GetObjectNamespace(t *testing.T) {
	ct := newTestClusterTunnel()
	adapter := ClusterTunnelAdapter{Tunnel: ct, Namespace: "operator-ns"}

	// The underlying ClusterTunnel is cluster-scoped, so its ObjectMeta.Namespace is ""
	if got := adapter.GetObject().GetNamespace(); got != "" {
		t.Errorf("GetObject().GetNamespace() = %q, want empty string (cluster-scoped)", got)
	}

	// But the adapter's GetNamespace returns the injected namespace
	if got := adapter.GetNamespace(); got != "operator-ns" {
		t.Errorf("adapter.GetNamespace() = %q, want %q", got, "operator-ns")
	}
}

func TestClusterTunnelAdapter_GetName(t *testing.T) {
	adapter := ClusterTunnelAdapter{Tunnel: newTestClusterTunnel(), Namespace: "operator-ns"}

	if got := adapter.GetName(); got != "cluster-tunnel" {
		t.Errorf("GetName() = %q, want %q", got, "cluster-tunnel")
	}
}

func TestClusterTunnelAdapter_GetUID(t *testing.T) {
	adapter := ClusterTunnelAdapter{Tunnel: newTestClusterTunnel(), Namespace: "operator-ns"}

	if got := adapter.GetUID(); got != types.UID("def-456") {
		t.Errorf("GetUID() = %q, want %q", got, "def-456")
	}
}

func TestClusterTunnelAdapter_GetSpec_GetStatus_SetStatus(t *testing.T) {
	adapter := ClusterTunnelAdapter{Tunnel: newTestClusterTunnel(), Namespace: "operator-ns"}

	spec := adapter.GetSpec()
	if spec.Cloudflare.Domain != "cluster.example.com" {
		t.Errorf("GetSpec().Cloudflare.Domain = %q, want %q", spec.Cloudflare.Domain, "cluster.example.com")
	}

	status := adapter.GetStatus()
	if status.TunnelId != "ctid-1" {
		t.Errorf("GetStatus().TunnelId = %q, want %q", status.TunnelId, "ctid-1")
	}

	newStatus := networkingv1alpha2.TunnelStatus{
		TunnelId:   "ctid-2",
		TunnelName: "updated-cluster-tunnel",
		AccountId:  "caid-2",
		ZoneId:     "czid-2",
	}
	adapter.SetStatus(newStatus)

	got := adapter.GetStatus()
	if got.TunnelId != "ctid-2" {
		t.Errorf("after SetStatus, GetStatus().TunnelId = %q, want %q", got.TunnelId, "ctid-2")
	}
	if got.TunnelName != "updated-cluster-tunnel" {
		t.Errorf("after SetStatus, GetStatus().TunnelName = %q, want %q", got.TunnelName, "updated-cluster-tunnel")
	}
}

func TestClusterTunnelAdapter_DeepCopyTunnel(t *testing.T) {
	adapter := ClusterTunnelAdapter{Tunnel: newTestClusterTunnel(), Namespace: "operator-ns"}

	copied := adapter.DeepCopyTunnel()

	if copied.GetName() != adapter.GetName() {
		t.Errorf("DeepCopyTunnel().GetName() = %q, want %q", copied.GetName(), adapter.GetName())
	}

	// Namespace should be preserved in the deep copy
	if copied.GetNamespace() != "operator-ns" {
		t.Errorf("DeepCopyTunnel().GetNamespace() = %q, want %q", copied.GetNamespace(), "operator-ns")
	}

	// Mutate the copy and verify the original is unchanged
	copied.SetStatus(networkingv1alpha2.TunnelStatus{TunnelId: "mutated"})
	if adapter.GetStatus().TunnelId != "ctid-1" {
		t.Errorf("original status mutated after DeepCopyTunnel; got TunnelId = %q, want %q", adapter.GetStatus().TunnelId, "ctid-1")
	}

	copied.SetLabels(map[string]string{"mutated": "true"})
	if _, ok := adapter.GetLabels()["mutated"]; ok {
		t.Errorf("original labels mutated after DeepCopyTunnel")
	}
}

// Verify both adapters satisfy the Tunnel interface at compile time.
var _ Tunnel = TunnelAdapter{}
var _ Tunnel = ClusterTunnelAdapter{}
