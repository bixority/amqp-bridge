# AMQP Message Bridge

A Rust service that bridges messages between two RabbitMQ instances with automatic reconnection and health checks.

## Features

- ✅ Automatic reconnection with exponential backoff
- ✅ Continuous recovery loop (5s interval)
- ✅ Health check endpoints for monitoring
- ✅ Python-compatible JSON logging
- ✅ At-least-once delivery guarantee
- ✅ Zero message loss with publisher confirmations

## Quick Start

### Prerequisites

- Rust 1.90+ (2024 edition)
- Two RabbitMQ instances

### Installation

```bash
git clone https://github.com/bixority/amqp-bridge
cd amqp-bridge
cargo build --release
```

### Configuration

Create a `.env` file:

```bash
# Required
SOURCE_DSN=amqp://user:password@source-host:5672/%2f
TARGET_DSN=amqp://user:password@target-host:5672/%2f

# Optional (with defaults)
SOURCE_QUEUE=old
TARGET_EXCHANGE=new_xchg
TARGET_ROUTING_KEY=update
HEALTH_PORT=8080
LOG_FORMAT=json  # or 'pretty'
RUST_LOG=info    # trace, debug, info, warn, error
```

### Run

```bash
cargo run --release
# or
./target/release/amqp-bridge
```

## Health Endpoints

- `GET /healthz` - Liveness probe
- `GET /ready` - Readiness probe
- `GET /startup` - Startup probe

Health states: `Starting` → `Healthy` / `Unhealthy`

## Logging Formats

### JSON (Production)
```bash
LOG_FORMAT=json cargo run
```
Python-compatible structured logs for log aggregation systems.

### Pretty (Development)
```bash
LOG_FORMAT=pretty cargo run
```
Human-readable colored output.

## Message Flow

```
Source Queue → Message Bridge → Target Exchange
       ↓
   Consume → Publish → Confirm → Acknowledge
```

**Guarantee**: Messages are acknowledged only after successful publish confirmation. Failed messages are nack'd and requeued.

## Error Handling

### Connection Categories
- `connection_refused` - Service not running or firewall blocking
- `access_refused` - Authentication failures
- `timeout` - No response from server
- `dns_resolution` - Hostname resolution failed

### Recovery Behavior
1. Initial connection: 10 retries with exponential backoff (1s → 30s)
2. Connection loss: Reconnect every 5 seconds
3. Consumer errors: Mark unhealthy, trigger reconnection
4. Publish failures: Nack with requeue

## Docker

```bash
# Build
docker build -t rabbitmq-bridge .

# Run
docker run -d \
  -e SOURCE_DSN="amqp://user:pass@source:5672/%2f" \
  -e TARGET_DSN="amqp://user:pass@target:5672/%2f" \
  -p 8080:8080 \
  rabbitmq-bridge
```

### Docker Compose

```bash
podman compose --env-file .env up --build --remove-orphans
```

## Project Structure

```
src/
├── main.rs      # Entry point, recovery loop
├── bridge.rs    # Message bridging logic
├── conf.rs      # Configuration
├── health.rs    # Health endpoints
└── logging.rs   # Logging setup
```

## Development

```bash
# Build
cargo build --release

# Test
cargo test

# Debug logging
RUST_LOG=debug cargo run
```

## Troubleshooting

Check logs for error categories and hints:
- **connection_refused**: Verify RabbitMQ is running and port is accessible
- **access_refused**: Check credentials and permissions
- **timeout**: Verify network connectivity
- **dns_resolution**: Check hostname or use IP address

Messages not appearing? Check:
1. Source queue has messages
2. Logs show `event="message_received"`
3. Target exchange exists
4. Routing key matches queue bindings

## Performance

- **QoS**: 1 (processes one message at a time)
- **Memory**: Minimal footprint
- **Recovery**: 5-second reconnection interval
- **Security**: Credentials sanitized in logs