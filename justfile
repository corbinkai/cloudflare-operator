set dotenv-load

project := "cloudflare-operator"
cluster_name := "koshee-cf-operator"
registry := "koshee-cf-operator-registry.localhost:16050"
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
    docker build -t {{image}} .

# Push the operator image to the k3d registry
docker-push:
    docker push {{image}}

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
      exit 0
    fi
    k3d cluster create --config k3d/cluster.yaml
    echo "Waiting for cluster to be ready..."
    kubectl wait --for=condition=Ready nodes --all --timeout=120s --context k3d-{{cluster_name}}

# Delete the k3d development cluster
delete-cluster:
    k3d cluster delete {{cluster_name}}

# Show cluster status
cluster-status:
    @k3d cluster list | head -1
    @k3d cluster list | grep {{cluster_name}} || echo "Cluster {{cluster_name}} not found"
    @echo ""
    @kubectl get nodes --context k3d-{{cluster_name}} 2>/dev/null || echo "Cannot reach cluster"

# ============================================================
# DEPLOYMENT
# ============================================================

# Install CRDs into the k3d cluster
install-crds:
    kubectl apply -f config/crd/bases/ --context k3d-{{cluster_name}}

# Uninstall CRDs from the k3d cluster
uninstall-crds:
    kubectl delete -f config/crd/bases/ --context k3d-{{cluster_name}} --ignore-not-found

# Deploy the operator to the k3d cluster
deploy: image install-crds
    #!/usr/bin/env bash
    set -e
    cd config/manager && ../../bin/kustomize edit set image controller={{image}}
    ../../bin/kustomize build config/default | kubectl apply --context k3d-{{cluster_name}} -f -
    echo "Waiting for operator deployment..."
    kubectl wait --for=condition=Available deployment/cloudflare-operator-controller-manager \
      -n cloudflare-operator-system --timeout=120s --context k3d-{{cluster_name}}

# Undeploy the operator from the k3d cluster
undeploy:
    ./bin/kustomize build config/default | kubectl delete --context k3d-{{cluster_name}} --ignore-not-found -f -

# ============================================================
# DEVELOPMENT WORKFLOW
# ============================================================

# Full setup: create cluster, build, deploy
dev: create-cluster deploy
    @echo ""
    @echo "Operator running in k3d-{{cluster_name}}"
    @echo "  kubectl --context k3d-{{cluster_name}} get pods -n cloudflare-operator-system"

# Rebuild and redeploy (fast iteration)
reload: image
    kubectl rollout restart deployment/cloudflare-operator-controller-manager \
      -n cloudflare-operator-system --context k3d-{{cluster_name}}
    kubectl rollout status deployment/cloudflare-operator-controller-manager \
      -n cloudflare-operator-system --context k3d-{{cluster_name}} --timeout=60s

# View operator logs
logs:
    kubectl logs -f deployment/cloudflare-operator-controller-manager \
      -n cloudflare-operator-system --context k3d-{{cluster_name}} -c manager

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
