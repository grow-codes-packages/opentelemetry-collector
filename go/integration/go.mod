module github.com/grow-codes-packages/opentelemetry-collector/go/integration

go 1.26

require github.com/grow-codes-packages/opentelemetry-collector/go v0.0.0

// The consumer module uses the package from this repository, exactly as a
// user would import it; docker/Dockerfile.go builds it this way.
replace github.com/grow-codes-packages/opentelemetry-collector/go => ../
