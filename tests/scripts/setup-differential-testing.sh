#!/usr/bin/env bash
# Setup script for Differential Testing V2
# Creates a reproducible environment for running differential tests
# between Apache Spark 4.1.1 and Thunderduck
#
# Compatible with both bash and zsh

set -e

# Detect shell and get script directory
if [ -n "$ZSH_VERSION" ]; then
    SCRIPT_DIR="$(cd "$(dirname "${(%):-%x}")" && pwd)"
elif [ -n "$BASH_VERSION" ]; then
    SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
else
    # Fallback for POSIX shells
    SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
fi
WORKSPACE_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
SPARK_VERSION="4.1.1"
# Vendor Spark inside the workspace under .spark/ (gitignored) so the install is
# self-contained and reproducible per-checkout. Override via SPARK_INSTALL_DIR.
SPARK_INSTALL_DIR="${SPARK_INSTALL_DIR:-$WORKSPACE_DIR/.spark}"
SPARK_HOME="$SPARK_INSTALL_DIR/spark-$SPARK_VERSION"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
NC='\033[0m'

echo -e "${BLUE}================================================================${NC}"
echo -e "${BLUE}Differential Testing V2 - Environment Setup${NC}"
echo -e "${BLUE}================================================================${NC}"
echo ""

# Venv configuration
VENV_DIR="${THUNDERDUCK_VENV_DIR:-$WORKSPACE_DIR/.venv}"

# ------------------------------------------------------------------------------
# Step 1: Check Java
# ------------------------------------------------------------------------------
echo -e "${BLUE}[1/5] Checking Java...${NC}"

if ! command -v java &> /dev/null; then
    echo -e "${RED}ERROR: Java is not installed${NC}"
    echo "Please install Java 17+ first:"
    echo "  Ubuntu/Debian: sudo apt-get install openjdk-17-jdk"
    echo "  Fedora: sudo dnf install java-17-openjdk"
    echo "  macOS: brew install openjdk@17"
    exit 1
fi

JAVA_VERSION=$(java -version 2>&1 | head -n 1 | awk -F '"' '{print $2}' | cut -d'.' -f1)
if [ -z "$JAVA_VERSION" ]; then
    JAVA_VERSION=$(java -version 2>&1 | head -n 1 | awk '{print $3}' | tr -d '"' | cut -d'.' -f1)
fi

echo -e "${GREEN}  Java version: $(java -version 2>&1 | head -n 1)${NC}"

if [ "$JAVA_VERSION" -lt 17 ] 2>/dev/null; then
    echo -e "${YELLOW}  WARNING: Java 17+ recommended, found version $JAVA_VERSION${NC}"
fi

# ------------------------------------------------------------------------------
# Step 2: Check Python and pip
# ------------------------------------------------------------------------------
echo -e "${BLUE}[2/5] Checking Python and pip...${NC}"

if ! command -v python3 &> /dev/null; then
    echo -e "${RED}ERROR: Python 3 is not installed${NC}"
    echo "Please install Python 3.8+ first"
    exit 1
fi

PYTHON_VERSION=$(python3 --version | awk '{print $2}')
echo -e "${GREEN}  Python version: $PYTHON_VERSION${NC}"

if ! command -v pip3 &> /dev/null && ! python3 -m pip --version &> /dev/null; then
    echo -e "${RED}ERROR: pip is not installed${NC}"
    echo "Please install pip first:"
    echo "  Ubuntu/Debian: sudo apt-get install python3-pip"
    echo "  Fedora: sudo dnf install python3-pip"
    echo "  macOS: python3 -m ensurepip"
    exit 1
fi

PIP_VERSION=$(python3 -m pip --version 2>/dev/null | awk '{print $2}')
echo -e "${GREEN}  pip version: $PIP_VERSION${NC}"

# ------------------------------------------------------------------------------
# Step 3: Create/reuse virtualenv
# ------------------------------------------------------------------------------
echo -e "${BLUE}[3/5] Setting up virtualenv at $VENV_DIR...${NC}"

if [ -d "$VENV_DIR" ] && [ -f "$VENV_DIR/bin/python3" ]; then
    echo -e "${GREEN}  Existing venv found, reusing${NC}"
else
    echo "  Creating virtualenv..."
    python3 -m venv "$VENV_DIR"
    echo -e "${GREEN}  Virtualenv created${NC}"
fi

# ------------------------------------------------------------------------------
# Step 4: Install Python dependencies into venv
# ------------------------------------------------------------------------------
echo -e "${BLUE}[4/5] Installing Python dependencies into venv...${NC}"

REQUIREMENTS_FILE="$WORKSPACE_DIR/tests/integration/requirements.txt"
if [ ! -f "$REQUIREMENTS_FILE" ]; then
    echo -e "${RED}ERROR: requirements.txt not found at $REQUIREMENTS_FILE${NC}"
    exit 1
fi

echo "  Installing from requirements.txt..."
"$VENV_DIR/bin/python3" -m pip install --quiet --upgrade pip
"$VENV_DIR/bin/python3" -m pip install --quiet -r "$REQUIREMENTS_FILE"

# Verify pyspark-client install (provides the `pyspark` Python module the diff
# tests import) by importing it from the venv.
PYSPARK_INSTALLED=$("$VENV_DIR/bin/python3" -c \
    'import pyspark; print(pyspark.__version__)' 2>/dev/null)
if [ -z "$PYSPARK_INSTALLED" ]; then
    echo -e "${RED}ERROR: pyspark-client installation failed (cannot import pyspark)${NC}"
    exit 1
fi
echo -e "${GREEN}  pyspark module version: $PYSPARK_INSTALLED${NC}"

# ------------------------------------------------------------------------------
# Step 5: Download and Install Apache Spark
# ------------------------------------------------------------------------------
echo -e "${BLUE}[5/5] Setting up Apache Spark ${SPARK_VERSION}...${NC}"

if [ -d "$SPARK_HOME" ] && [ -f "$SPARK_HOME/bin/spark-submit" ]; then
    echo -e "${GREEN}  Spark already installed at: $SPARK_HOME${NC}"
else
    echo "  Downloading Apache Spark ${SPARK_VERSION}..."

    mkdir -p "$SPARK_INSTALL_DIR"
    SPARK_TARBALL="spark-${SPARK_VERSION}-bin-hadoop3.tgz"
    # Try the current-release mirror first; fall back to the archive for older releases.
    SPARK_URL_PRIMARY="https://dlcdn.apache.org/spark/spark-${SPARK_VERSION}/${SPARK_TARBALL}"
    SPARK_URL_ARCHIVE="https://archive.apache.org/dist/spark/spark-${SPARK_VERSION}/${SPARK_TARBALL}"

    # Download to temp directory
    TEMP_DIR=$(mktemp -d)
    cd "$TEMP_DIR"

    if ! curl -fsSL "$SPARK_URL_PRIMARY" -o "$SPARK_TARBALL"; then
        echo "  Primary mirror missing ${SPARK_VERSION}, trying archive…"
        if ! curl -fsSL "$SPARK_URL_ARCHIVE" -o "$SPARK_TARBALL"; then
            echo -e "${RED}ERROR: Failed to download Spark from either mirror${NC}"
            echo "  Tried:"
            echo "    $SPARK_URL_PRIMARY"
            echo "    $SPARK_URL_ARCHIVE"
            rm -rf "$TEMP_DIR"
            exit 1
        fi
    fi

    echo "  Extracting Spark..."
    tar -xzf "$SPARK_TARBALL"
    mv "spark-${SPARK_VERSION}-bin-hadoop3" "$SPARK_HOME"

    # Cleanup. Leave the deleted temp dir before removing it — otherwise the
    # shell's cwd points at a nonexistent directory and subsequent commands
    # (notably `spark-submit`, which resolves relative paths internally) fail.
    cd "$WORKSPACE_DIR"
    rm -rf "$TEMP_DIR"

    echo -e "${GREEN}  Spark installed at: $SPARK_HOME${NC}"
fi

# Create symlink for convenience
ln -sfn "$SPARK_HOME" "$SPARK_INSTALL_DIR/current"

# Verify Spark installation
if ! "$SPARK_HOME/bin/spark-submit" --version &>/dev/null; then
    echo -e "${RED}ERROR: Spark installation verification failed${NC}"
    exit 1
fi

echo -e "${GREEN}  Spark version: $("$SPARK_HOME/bin/spark-submit" --version 2>&1 | grep -i version | head -1)${NC}"

# ------------------------------------------------------------------------------
# Write environment configuration
# ------------------------------------------------------------------------------
# Building Thunderduck is the developer's job (`cargo build --release`); this
# script is environment setup only.
ENV_FILE="$WORKSPACE_DIR/tests/integration/.env"
cat > "$ENV_FILE" << EOF
# Differential Testing V2 Environment Configuration
# Generated by setup-differential-testing.sh on $(date)

export SPARK_HOME="$SPARK_HOME"
export SPARK_VERSION="$SPARK_VERSION"
export WORKSPACE_DIR="$WORKSPACE_DIR"
export THUNDERDUCK_VENV_DIR="$VENV_DIR"
EOF

echo ""
echo -e "${GREEN}================================================================${NC}"
echo -e "${GREEN}Setup Complete!${NC}"
echo -e "${GREEN}================================================================${NC}"
echo ""
echo -e "Configuration saved to: ${BLUE}$ENV_FILE${NC}"
echo ""
echo -e "Virtualenv: ${BLUE}$VENV_DIR${NC}"
echo ""
echo -e "${YELLOW}To run differential tests:${NC}"
echo ""
echo "  # Build the Rust release binary first:"
echo "  cargo build --release"
echo ""
echo "  # Cargo test target (one #[test] per suite, all #[ignore]):"
echo "  cargo test -p thunderduck-connect-server --test differential -- --ignored --nocapture"
echo ""
echo "  # Or call the bash runner directly:"
echo "  $SCRIPT_DIR/run-differential-tests.sh tpch"
echo ""
echo -e "${GREEN}================================================================${NC}"
