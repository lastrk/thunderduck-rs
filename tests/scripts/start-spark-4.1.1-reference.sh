#!/usr/bin/env bash
# Start Apache Spark 4.1.1 Connect server on port 15003 for differential testing
#
# Compatible with both bash and zsh

set -e

SPARK_HOME="${SPARK_HOME:-$HOME/spark/current}"
SPARK_VERSION="4.1.1"
SPARK_PORT="${SPARK_PORT:-15003}"
SPARK_WAREHOUSE_DIR="${SPARK_WAREHOUSE_DIR:-}"
SPARK_DRIVER_MEMORY="${SPARK_DRIVER_MEMORY:-4g}"
SPARK_MASTER="${SPARK_MASTER:-local[*]}"
SPARK_AQE_ENABLED="${SPARK_AQE_ENABLED:-false}"
SPARK_BROADCAST_THRESHOLD="${SPARK_BROADCAST_THRESHOLD:--1}"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

echo -e "${BLUE}================================================================${NC}"
echo -e "${BLUE}Starting Apache Spark ${SPARK_VERSION} Connect Reference Server${NC}"
echo -e "${BLUE}================================================================${NC}"

# Check if Spark is installed
if [ ! -d "$SPARK_HOME" ]; then
    echo -e "${RED}ERROR: Spark not found at $SPARK_HOME${NC}"
    exit 1
fi

echo -e "${GREEN}Using Spark at: $SPARK_HOME${NC}"

# Check if server is already running
if pgrep -f "org.apache.spark.sql.connect.service.SparkConnectServer.*${SPARK_PORT}" > /dev/null; then
    echo -e "${GREEN}✓ Spark Connect server already running on port ${SPARK_PORT}${NC}"
    exit 0
fi


# Create log directory
SPARK_LOG_DIR="${SPARK_HOME}/work/logs"
mkdir -p "$SPARK_LOG_DIR"

echo -e "${BLUE}Starting Spark Connect server on port ${SPARK_PORT}...${NC}"

# Set required JVM options for Apache Arrow (Spark 4.1.x requirement)
export SPARK_SUBMIT_OPTS="--add-opens=java.base/java.nio=ALL-UNNAMED"

# Build warehouse dir argument if provided
WAREHOUSE_CONF=""
if [ -n "$SPARK_WAREHOUSE_DIR" ]; then
    echo -e "${BLUE}Using custom warehouse dir: ${SPARK_WAREHOUSE_DIR}${NC}"
    WAREHOUSE_CONF="--conf spark.sql.warehouse.dir=${SPARK_WAREHOUSE_DIR}"
fi

# Start Spark Connect server in the background.
#
# We explicitly use & because K8s Spark images run start-connect-server.sh in
# the foreground (no daemon fork), which would block our script indefinitely.
# Standard Spark distributions daemonize internally — the extra & is harmless
# in that case (the shell exits immediately, and the JVM is already orphaned).
#
# Startup tuning for single-node local mode:
#   spark.ui.enabled=false                              - skip Jetty web UI initialization
#   spark.ui.showConsoleProgress=false                  - disable console progress bar
#   spark.sql.catalogImplementation=in-memory           - skip Hive/Derby metastore initialization
#   spark.metrics.enabled=false                         - disable metrics system
#   spark.sql.streaming.forceDeleteTempCheckpointLocation=true - skip stale checkpoint checks
#
"$SPARK_HOME/sbin/start-connect-server.sh" \
    --master "${SPARK_MASTER}" \
    --driver-memory ${SPARK_DRIVER_MEMORY} \
    --conf spark.driver.host=localhost \
    --conf spark.driver.bindAddress=127.0.0.1 \
    --conf spark.connect.grpc.binding.port=${SPARK_PORT} \
    --conf spark.sql.shuffle.partitions=4 \
    --conf spark.sql.adaptive.enabled=${SPARK_AQE_ENABLED} \
    --conf spark.sql.autoBroadcastJoinThreshold=${SPARK_BROADCAST_THRESHOLD} \
    --conf spark.ui.enabled=false \
    --conf spark.ui.showConsoleProgress=false \
    --conf spark.sql.catalogImplementation=in-memory \
    --conf spark.metrics.enabled=false \
    --conf spark.sql.streaming.forceDeleteTempCheckpointLocation=true \
    ${WAREHOUSE_CONF} \
    > "${SPARK_LOG_DIR}/start.log" 2>&1 &

# Server is backgrounded — caller (DualServerManager.wait_for_port) handles readiness via TCP socket polling.
echo -e "${GREEN}Spark Connect server process started on port ${SPARK_PORT}${NC}"
echo -e "${BLUE}Logs: ${SPARK_LOG_DIR}/start.log${NC}"
echo -e "${BLUE}================================================================${NC}"
exit 0
