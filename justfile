set dotenv-load

project := "cloudflare-operator"
cluster_name := "koshee-cf-operator"
kube_ctx := "k3d-" + cluster_name
kubeconfig := justfile_directory() + "/kubeconfig-k3d.yaml"
registry_host := "localhost:9050"
registry := "koshee-dev-zot:5000"
image := registry + "/" + project + ":dev"

# ============================================================
# BUILD & DEVELOPMENT
# ============================================================

# Build the operator binary
build:
    go build -o bin/manager cmd/main.go

# Run the operator locally against current kubeconfig
run:
    go run ./cmd/main.go

# Format Go code
fmt:
    go fmt ./...

# Run go vet
vet:
    go vet ./...

# ============================================================
# CODE GENERATION
# ============================================================

# Regenerate CRDs, RBAC, and webhook configs
manifests:
    ./bin/controller-gen rbac:roleName=manager-role crd webhook paths="./..." output:crd:artifacts:config=config/crd/bases

# Regenerate DeepCopy methods
generate:
    ./bin/controller-gen object:headerFile="hack/boilerplate.go.txt" paths="./..."

# ============================================================
# TESTING
# ============================================================

# Run unit and integration tests
test:
    #!/usr/bin/env bash
    set -e
    KUBEBUILDER_ASSETS="$(./bin/setup-envtest use 1.31.0 --bin-dir ./bin -p path 2>/dev/null)" \
      go test $(go list ./... | grep -v /e2e) -coverprofile cover.out
    echo ""
    echo "Coverage:"
    go tool cover -func cover.out | grep "^total:"

# Run a single test by name
test-one NAME:
    go test ./internal/controller/ -run {{NAME}} -v -count=1

# Run tests with verbose output
test-v:
    go test ./... -v -count=1 -short

# Run e2e tests against the k3d cluster
test-e2e:
    go test ./test/e2e/ -v -ginkgo.v

# ============================================================
# LINTING
# ============================================================

# Lint new changes only (vs main branch)
lint:
    ./bin/golangci-lint run --new-from-rev main

# Lint entire codebase
lint-full:
    ./bin/golangci-lint run

# Auto-fix lint issues
lint-fix:
    ./bin/golangci-lint run --fix

# ============================================================
# DOCKER
# ============================================================

# Build the operator container image
docker-build:
    docker build -t {{registry_host}}/{{project}}:dev .

# Push the operator image to the k3d registry
docker-push:
    skopeo copy --tmpdir /tmp --dest-tls-verify=false docker-daemon:{{registry_host}}/{{project}}:dev docker://{{registry_host}}/{{project}}:dev

# Build and push in one step
image: docker-build docker-push

# ============================================================
# K3D CLUSTER
# ============================================================

# Create the k3d development cluster
create-cluster:
    #!/usr/bin/env bash
    set -e
    if k3d cluster list 2>/dev/null | grep -qw {{cluster_name}}; then
      echo "Cluster {{cluster_name}} already exists"
    else
      k3d cluster create --config k3d/cluster.yaml
    fi
    k3d kubeconfig get {{cluster_name}} > {{kubeconfig}}
    echo "Waiting for cluster to be ready..."
    KUBECONFIG={{kubeconfig}} kubectl wait --for=condition=Ready nodes --all --timeout=120s --context {{kube_ctx}}

# Delete the k3d development cluster
delete-cluster:
    k3d cluster delete {{cluster_name}}

# Show cluster status
cluster-status:
    @k3d cluster list | head -1
    @k3d cluster list | grep {{cluster_name}} || echo "Cluster {{cluster_name}} not found"
    @echo ""
    @KUBECONFIG={{kubeconfig}} kubectl get nodes --context {{kube_ctx}} 2>/dev/null || echo "Cannot reach cluster"

# ============================================================
# DEPLOYMENT
# ============================================================

# Install CRDs into the k3d cluster
install-crds:
    KUBECONFIG={{kubeconfig}} kubectl apply -f config/crd/bases/ --context {{kube_ctx}}

# Uninstall CRDs from the k3d cluster
uninstall-crds:
    KUBECONFIG={{kubeconfig}} kubectl delete -f config/crd/bases/ --context {{kube_ctx}} --ignore-not-found

# Deploy the operator to the k3d cluster
deploy: image install-crds
    #!/usr/bin/env bash
    set -e
    KUSTOMIZE_BIN="./bin/kustomize"
    if [ ! -x "$KUSTOMIZE_BIN" ]; then
      KUSTOMIZE_BIN="$(command -v kustomize)"
    fi
    (cd config/manager && "$KUSTOMIZE_BIN" edit set image controller={{image}})
    "$KUSTOMIZE_BIN" build --load-restrictor LoadRestrictionsNone config/local | KUBECONFIG={{kubeconfig}} kubectl apply --context {{kube_ctx}} -f -
    echo "Waiting for operator deployment..."
    KUBECONFIG={{kubeconfig}} kubectl wait --for=condition=Available deployment/cloudflare-operator-controller-manager \
      -n cloudflare-operator-system --timeout=120s --context {{kube_ctx}}

# Undeploy the operator from the k3d cluster
undeploy:
    #!/usr/bin/env bash
    set -e
    KUSTOMIZE_BIN="./bin/kustomize"
    if [ ! -x "$KUSTOMIZE_BIN" ]; then
      KUSTOMIZE_BIN="$(command -v kustomize)"
    fi
    "$KUSTOMIZE_BIN" build --load-restrictor LoadRestrictionsNone config/local | KUBECONFIG={{kubeconfig}} kubectl delete --context {{kube_ctx}} --ignore-not-found -f -

# ============================================================
# DEVELOPMENT WORKFLOW
# ============================================================

# Full setup: create cluster, build, deploy
dev: create-cluster deploy
    @echo ""
    @echo "Operator running in {{kube_ctx}}"
    @echo "  KUBECONFIG={{kubeconfig}} kubectl --context {{kube_ctx}} get pods -n cloudflare-operator-system"

shared-up: dev

standalone-up: dev

destroy: undeploy delete-cluster

# Rebuild and redeploy (fast iteration)
reload: image
    KUBECONFIG={{kubeconfig}} kubectl rollout restart deployment/cloudflare-operator-controller-manager \
      -n cloudflare-operator-system --context {{kube_ctx}}
    KUBECONFIG={{kubeconfig}} kubectl rollout status deployment/cloudflare-operator-controller-manager \
      -n cloudflare-operator-system --context {{kube_ctx}} --timeout=60s

# View operator logs
logs:
    KUBECONFIG={{kubeconfig}} kubectl logs -f deployment/cloudflare-operator-controller-manager \
      -n cloudflare-operator-system --context {{kube_ctx}} -c manager

# ============================================================
# CI
# ============================================================

# Run the full CI check (fmt, vet, lint, test)
ci: fmt vet lint-full test

# ============================================================
# CLEANUP
# ============================================================

# Remove build artifacts
clean:
    rm -rf bin/manager cover.out dist/

# Remove everything (cluster + artifacts)
clean-all: delete-cluster clean

# ============================================================
# E2E TESTING
# ============================================================

# Build and push the mock CF server image
build-cfmock:
    docker build -t {{registry_host}}/cfmock-server:dev -f test/e2e/Dockerfile.cfmock .
    skopeo copy --tmpdir /tmp --dest-tls-verify=false docker-daemon:{{registry_host}}/cfmock-server:dev docker://{{registry_host}}/cfmock-server:dev

# Deploy the mock CF server to the cluster
deploy-cfmock: build-cfmock
    KUBECONFIG={{kubeconfig}} kubectl apply -f test/e2e/manifests/cfmock-deployment.yaml --context {{kube_ctx}}
    KUBECONFIG={{kubeconfig}} kubectl wait --for=condition=Available deployment/cfmock-server \
      -n cloudflare-operator-system --timeout=60s --context {{kube_ctx}}

# Patch the operator to use the mock CF server
patch-operator-cfmock:
    KUBECONFIG={{kubeconfig}} kubectl set env deployment/cloudflare-operator-controller-manager \
      -n cloudflare-operator-system \
      -c manager \
      CLOUDFLARE_API_BASE_URL=http://cfmock-server.cloudflare-operator-system:8080/client/v4 \
      --context {{kube_ctx}}
    KUBECONFIG={{kubeconfig}} kubectl rollout status deployment/cloudflare-operator-controller-manager \
      -n cloudflare-operator-system --timeout=60s --context {{kube_ctx}}

# Run e2e tests (requires cluster + operator + cfmock deployed)
e2e: deploy-cfmock patch-operator-cfmock
    go test ./test/e2e/ -v -timeout 5m

# Full e2e setup and run
e2e-full: dev deploy-cfmock patch-operator-cfmock
    go test ./test/e2e/ -v -timeout 5m

# ============================================================
# RUST BUILD
# ============================================================

# Build Rust operator binary
rust-build:
    cargo build --release

# Run Rust tests
rust-test:
    cargo test

# Run Rust linting
rust-lint:
    cargo clippy -- -D warnings

# Check Rust formatting
rust-fmt:
    cargo fmt --check

# Fix Rust formatting
rust-fmt-fix:
    cargo fmt

# Generate CRD YAML from Rust types
rust-crdgen:
    cargo run --bin crdgen

# Build Rust operator Docker image
rust-docker-build:
    docker build -t {{registry_host}}/{{project}}:dev -f Dockerfile.rust .

# Push Rust operator Docker image
rust-docker-push:
    skopeo copy --tmpdir /tmp --dest-tls-verify=false docker-daemon:{{registry_host}}/{{project}}:dev docker://{{registry_host}}/{{project}}:dev

# Build and push Rust image
rust-image: rust-docker-build rust-docker-push

# Run full Rust CI
rust-ci: rust-fmt rust-lint rust-test
