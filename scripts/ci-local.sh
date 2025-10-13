#!/bin/bash
# Local CI runner using act to reproduce GitHub Actions environment
#
# This script uses 'act' to run the actual GitHub Actions workflow locally
# in Docker containers, ensuring exact reproduction of the CI environment.
#
# Prerequisites:
#   - Docker (for act to run containers)
#   - act (install via: brew install act)
#
# Usage: ./scripts/ci-local.sh [job-name]
#
# Examples:
#   ./scripts/ci-local.sh           # Run all jobs
#   ./scripts/ci-local.sh format    # Run only format check
#   ./scripts/ci-local.sh test      # Run only tests

set -e

# Colors for output
GREEN='\033[0;32m'
RED='\033[0;31m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

echo "========================================"
echo "Local CI Runner (using act)"
echo "========================================"
echo ""

# Check prerequisites
if ! command -v act &> /dev/null; then
    echo -e "${RED}Error: 'act' is not installed${NC}"
    echo ""
    echo "Install act with:"
    echo "  brew install act"
    echo ""
    echo "Or see: https://github.com/nektos/act"
    exit 1
fi

if ! command -v docker &> /dev/null; then
    echo -e "${RED}Error: Docker is not installed or not running${NC}"
    echo ""
    echo "Install Docker Desktop from: https://www.docker.com/products/docker-desktop"
    exit 1
fi

# Check if Docker daemon is running
if ! docker info &> /dev/null; then
    echo -e "${RED}Error: Docker daemon is not running${NC}"
    echo ""
    echo "Please start Docker Desktop"
    exit 1
fi

# Parse arguments
JOB_NAME="${1:-}"

# Detect architecture and set container platform if needed
ARCH_FLAG=""
if [[ $(uname -m) == "arm64" ]]; then
    echo -e "${YELLOW}Detected Apple Silicon, using linux/amd64 architecture${NC}"
    echo ""
    ARCH_FLAG="--container-architecture linux/amd64"
fi

if [ -n "$JOB_NAME" ]; then
    echo -e "${BLUE}Running job: ${JOB_NAME}${NC}"
    echo ""
    act push -j "$JOB_NAME" $ARCH_FLAG
else
    echo -e "${BLUE}Running all CI jobs${NC}"
    echo ""
    echo -e "${YELLOW}Note: This will run all jobs including those requiring PostgreSQL${NC}"
    echo -e "${YELLOW}Jobs with database dependencies will spin up PostgreSQL containers${NC}"
    echo ""

    # List available jobs first
    echo "Available jobs:"
    act -l
    echo ""

    # Run all jobs
    act push $ARCH_FLAG
fi

EXIT_CODE=$?

echo ""
echo "========================================"
if [ $EXIT_CODE -eq 0 ]; then
    echo -e "${GREEN}✓ CI passed locally${NC}"
else
    echo -e "${RED}✗ CI failed locally${NC}"
fi
echo "========================================"

exit $EXIT_CODE
