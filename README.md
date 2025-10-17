# AMQP message bridge

A reliable, production-ready Rust service that bridges messages between two RabbitMQ instances with automatic reconnection, health checks, and structured logging.

## Why Use This?

### Common Use Cases

- **RabbitMQ Migration**: Safely migrate messages from an old RabbitMQ cluster to a new one without downtime
- **Message Replication**: Replicate messages between different environments (staging → production, on-prem → cloud)
- **Queue Consolidation**: Bridge messages from legacy queues to modernized exchanges
- **Cross-Datacenter Sync**: Keep message queues synchronized across different data centers
- **Version Upgrades**: Facilitate zero-downtime RabbitMQ version upgrades by bridging old and new clusters

### Key Features

- ✅ **Automatic Reconnection**: Recovers from connection failures with exponential backoff
- ✅ **Health Checks**: Built-in liveness and readiness endpoints for Kubernetes/Docker
- ✅ **Structured Logging**: JSON logs ready for Loki/Grafana monitoring
- ✅ **At-Least-Once Delivery**: Messages are only acknowledged after successful publish and confirmation
- ✅ **Connection Monitoring**: Detects and logs connection issues with helpful diagnostics
- ✅ **Zero Message Loss**: Uses publisher confirmations and manual acknowledgments
- ✅ **Production Ready**: Battle-tested error handling and graceful shutdown

## Quick Start

### Prerequisites

- Rust 1.90+ (2024 edition)
- Two RabbitMQ instances (source and target)

### Installation

```bash
git clone https://github.com/bixority/amqp-bridge
cd amqp-bridge
cargo build --release
```

### Basic Usage

1. Create a `.env` file:

```bash
# Source RabbitMQ (where messages come from)
SOURCE_DSN=amqp://user:password@source-host:5672/%2f
SOURCE_QUEUE=old_queue

# Target RabbitMQ (where messages go to)
TARGET_DSN=amqp://user:password@target-host:5672/%2f
TARGET_EXCHANGE=new_exchange
TARGET_ROUTING_KEY=routing.key

# Health check server port (optional, default: 8080)
HEALTH_PORT=8080

# Logging format (optional, default: pretty)
LOG_FORMAT=json  # or 'pretty' for human-readable logs
RUST_LOG=info    # trace, debug, info, warn, error
```

2. Run the bridge:

```bash
cargo run --release
```

Or using the binary directly:

```bash
./target/release/amqp-bridge
```

## Configuration

All configuration is done via environment variables:

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `SOURCE_DSN` | **Yes** | - | AMQP connection string for source RabbitMQ |
| `SOURCE_QUEUE` | No | `old` | Queue name to consume messages from |
| `TARGET_DSN` | **Yes** | - | AMQP connection string for target RabbitMQ |
| `TARGET_EXCHANGE` | No | `new_xchg` | Exchange to publish messages to |
| `TARGET_ROUTING_KEY` | No | `update` | Routing key for published messages |
| `HEALTH_PORT` | No | `8080` | Port for health check endpoints |
| `LOG_FORMAT` | No | `pretty` | Logging format: `json` or `pretty` |
| `RUST_LOG` | No | `info` | Log level: `trace`, `debug`, `info`, `warn`, `error` |

### AMQP Connection String Format

```
amqp://username:password@host:port/vhost
```

Example:
```
amqp://admin:secret@rabbitmq.example.com:5672/%2f
```

Note: Use `%2f` for the default vhost `/`

## Health Checks

The bridge exposes health check endpoints on the configured `HEALTH_PORT`:

### Endpoints

- **`GET /health/live`** - Liveness probe (returns 200 if service is running)
- **`GET /health/ready`** - Readiness probe (returns 200 if connected and ready to process)

### Kubernetes Example

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: rabbitmq-bridge
spec:
  replicas: 1
  template:
    spec:
      containers:
      - name: bridge
        image: rabbitmq-bridge:latest
        env:
        - name: SOURCE_DSN
          valueFrom:
            secretKeyRef:
              name: rabbitmq-secrets
              key: source-dsn
        - name: TARGET_DSN
          valueFrom:
            secretKeyRef:
              name: rabbitmq-secrets
              key: target-dsn
        - name: LOG_FORMAT
          value: "json"
        - name: RUST_LOG
          value: "info"
        ports:
        - containerPort: 8080
          name: health
        livenessProbe:
          httpGet:
            path: /health/live
            port: 8080
          initialDelaySeconds: 10
          periodSeconds: 30
        readinessProbe:
          httpGet:
            path: /health/ready
            port: 8080
          initialDelaySeconds: 5
          periodSeconds: 10
```

## Docker Deployment

### Using Docker

```bash
# Build image
docker build -t rabbitmq-bridge .

# Run container
docker run -d \
  --name rabbitmq-bridge \
  -e SOURCE_DSN="amqp://user:pass@source:5672/%2f" \
  -e SOURCE_QUEUE="old_queue" \
  -e TARGET_DSN="amqp://user:pass@target:5672/%2f" \
  -e TARGET_EXCHANGE="new_exchange" \
  -e LOG_FORMAT="json" \
  -p 8080:8080 \
  rabbitmq-bridge
```

### Docker Compose

```yaml
version: '3.8'

services:
  rabbitmq-bridge:
    image: rabbitmq-bridge:latest
    environment:
      SOURCE_DSN: "amqp://guest:guest@source-rabbitmq:5672/%2f"
      SOURCE_QUEUE: "old_queue"
      TARGET_DSN: "amqp://guest:guest@target-rabbitmq:5672/%2f"
      TARGET_EXCHANGE: "new_exchange"
      TARGET_ROUTING_KEY: "routing.key"
      LOG_FORMAT: "json"
      RUST_LOG: "info"
    ports:
      - "8080:8080"
    restart: unless-stopped
    depends_on:
      - source-rabbitmq
      - target-rabbitmq

  source-rabbitmq:
    image: rabbitmq:3.6-management
    ports:
      - "5672:5672"
      - "15672:15672"

  target-rabbitmq:
    image: rabbitmq:3.13-management
    ports:
      - "5673:5672"
      - "15673:15672"
```

## Logging

The bridge supports two logging formats:

### Pretty Format (Development)

Human-readable, colored output for local development:

```bash
LOG_FORMAT=pretty cargo run
```

Output:
```
2025-10-17T15:55:15.628822Z  INFO amqp_bridge: Starting MessageBridge initialization source_queue="old_queue"
2025-10-17T15:55:15.829103Z  INFO source_rabbitmq: Successfully connected attempt=1
2025-10-17T15:55:16.142567Z  INFO amqp_bridge: Received message message_size=1024 delivery_tag=1
```

### JSON Format (Production)

Structured JSON logs for Loki, Grafana, or any log aggregation system:

```bash
LOG_FORMAT=json cargo run
```

Output:
```json
{"timestamp":"2025-10-17T15:55:15.628822Z","level":"INFO","target":"amqp_bridge","fields":{"source_queue":"old_queue"},"message":"Starting MessageBridge initialization"}
{"timestamp":"2025-10-17T15:55:15.829103Z","level":"INFO","target":"source_rabbitmq","fields":{"attempt":1,"status":"success"},"message":"Successfully connected"}
```

### Grafana Loki Queries

Example queries for monitoring:

```logql
# All errors
{job="rabbitmq-bridge"} | json | level="ERROR"

# Connection failures
{job="rabbitmq-bridge"} | json | error_type="connection_refused"

# Message throughput
rate({job="rabbitmq-bridge"} | json | event="message_published" [5m])

# Authentication issues
{job="rabbitmq-bridge"} | json | error_type="access_refused"
```

## Message Flow

```
┌─────────────────┐         ┌──────────────────┐         ┌─────────────────┐
│  Source Queue   │────────>│  Message Bridge  │────────>│ Target Exchange │
│   (RabbitMQ)    │         │                  │         │   (RabbitMQ)    │
└─────────────────┘         └──────────────────┘         └─────────────────┘
                                     │
                                     │
                            ┌────────▼────────┐
                            │  Health Check   │
                            │   :8080/health  │
                            └─────────────────┘
```

### Processing Guarantees

1. **Consume** message from source queue
2. **Publish** message to target exchange
3. **Wait** for publisher confirmation
4. **Acknowledge** source message only after successful confirmation
5. If any step fails, message is **nack'd** and requeued

This ensures **at-least-once delivery** - messages may be delivered multiple times but will never be lost.

## Troubleshooting

### Connection Refused

```
ERROR: Connection refused: RabbitMQ is not accepting connections
```

**Solutions:**
- Verify RabbitMQ service is running: `systemctl status rabbitmq-server`
- Check firewall rules allow connections to port 5672
- Ensure correct host and port in DSN

### Authentication Failed

```
ERROR: ACCESS_REFUSED - Login was refused using authentication mechanism PLAIN
```

**Solutions:**
- Verify username and password are correct
- Check user exists: `rabbitmqctl list_users`
- Verify user has permissions: `rabbitmqctl list_permissions -p /`
- Grant permissions: `rabbitmqctl set_permissions -p / username ".*" ".*" ".*"`

### Messages Not Appearing

1. Check source queue has messages: RabbitMQ Management UI → Queues
2. Verify bridge is consuming: Check logs for `event="message_received"`
3. Confirm target exchange exists and is correct type
4. Check routing key matches target queue bindings

### High Memory Usage

The bridge processes one message at a time (QoS=1) to prevent memory issues. If still experiencing problems:
- Monitor message size in logs
- Check for message backlog in source queue
- Consider scaling horizontally with multiple bridge instances

## Development

### Building from Source

```bash
# Development build
cargo build

# Release build (optimized)
cargo build --release

# Run tests
cargo test

# Run with debug logging
RUST_LOG=debug cargo run
```

### Project Structure

```
.
├── src/
│   ├── main.rs           # Application entry point
│   ├── bridge.rs         # Core message bridging logic
│   ├── conf.rs           # Configuration management
│   ├── health.rs         # Health check endpoints
│   └── logging.rs        # Logging configuration
├── Cargo.toml
├── Dockerfile
└── README.md
```

## Performance

- **Throughput**: Processes messages as fast as RabbitMQ can deliver (QoS=1)
- **Memory**: Minimal footprint,


Run in compose file:
```shell
podman compose --env-file .env up --build --remove-orphans
```
