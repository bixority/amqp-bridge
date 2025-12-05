# AMQP Message Bridge

A service that bridges messages between two AMQP instances with automatic reconnection and health checks.
Also available as an extendable library (crate) so other codebases can plug in a custom message transformer and run the bridge.

## Features

- ✅ Automatic reconnection with exponential backoff
- ✅ Continuous recovery loop (5s interval)
- ✅ Health check endpoints for monitoring
- ✅ Python-compatible JSON logging
- ✅ At-least-once delivery guarantee
- ✅ Zero message loss with publisher confirmations
- ✅ Extensible: plug in your own async message transformer

## Quick Start

### Prerequisites

- Rust 1.90+ (2024 edition)
- Two AMQP instances

### Installation

```bash
git clone https://github.com/bixority/amqp-bridge
cd amqp-bridge
cargo build --release
```

Alternatively, build a static Linux binary via Makefile (musl target):

```bash
make release
# output: target/amqp-bridge
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

## Podman

```bash
# Build
podman build -t amqp-bridge .

# Run
podman run -d \
  -e SOURCE_DSN="amqp://user:pass@source:5672/%2f" \
  -e TARGET_DSN="amqp://user:pass@target:5672/%2f" \
  -p 8080:8080 \
  amqp-bridge
```

### Compose file

```bash
podman compose --env-file .env up --build --remove-orphans
```

## Project Structure

```
src/
├── main.rs      # Entry point; delegates to recovery runner
├── bridge.rs    # Message bridging logic
├── conf.rs      # Configuration
├── health.rs    # Health endpoints
├── logging.rs   # Logging setup
└── transform.rs # Transformer trait and helper types

## Using as a Library (Extendable Crate)

You can depend on this crate and provide your own message transformation logic.

Implement a transformer and run the bridge:

```rust
use std::sync::Arc;
use anyhow::Result;
use amqp_bridge::{
    Config,
    HealthState,
    MessageBridge,
    MessageTransformer,
    Message,
};
use async_trait::async_trait;
use tokio::sync::RwLock;

struct MyTransformer;

#[async_trait]
impl MessageTransformer for MyTransformer {
    async fn transform(&self, input: Message) -> Result<Message> {
        // Example: uppercase the body if it's UTF-8
        let data = match String::from_utf8(input.data) {
            Ok(s) => s.to_uppercase().into_bytes(),
            Err(e) => e.into_bytes(),
        };
        Ok(Message { data, properties: input.properties })
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Load config from env the same way the binary does
    let config = Config::from_env()?;
    let health_state = Arc::new(RwLock::new(HealthState::default()));

    // Create bridge with optional transformer and run
    let transformer: Arc<dyn MessageTransformer> = Arc::new(MyTransformer);
    let bridge = MessageBridge::new(
        config.clone(),
        health_state.clone(),
        Some(transformer),
    ).await?;

    bridge.run().await?;
    Ok(())
}
```

Alternatively, use the convenience runners with auto-recovery and Ctrl+C handling:

```rust
use std::sync::Arc;
use amqp_bridge::{
    Config,
    HealthState,
    run_with_ctrl_c,
    MessageTransformer,
};
use tokio::sync::RwLock;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config::from_env()?;
    let health_state = Arc::new(RwLock::new(HealthState::default()));
    let transformer: Arc<dyn MessageTransformer> = Arc::new(MyTransformer);
    // Pass Some(transformer) to enable transformation
    run_with_ctrl_c(config, health_state, Some(transformer)).await
}
```

Notes:
- The transformer runs for each consumed message before it is published to the target.
- On transformer error, the message will be nack'ed with requeue to avoid loss.

Add this crate to your project (if not using a local path), for example via Git:

```toml
[dependencies]
amqp-bridge = { git = "https://github.com/bixority/amqp-bridge" }
```

To run with pass-through behavior (no transform), just don't pass a transformer:

```rust
use amqp_bridge::{Config, HealthState, run_with_ctrl_c};
use tokio::sync::RwLock;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config::from_env()?;
    let health_state = Arc::new(RwLock::new(HealthState::default()));
    // None means: do not transform; forward as-is
    run_with_ctrl_c(config, health_state, None).await
}
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

## Networking & Health Server

- The health server binds to 0.0.0.0:HEALTH_PORT (default 8080), exposing:
  - GET /healthz (liveness)
  - GET /ready (readiness)
  - GET /startup (startup)

This makes it suitable for container/Kubernetes probes out of the box.

## Troubleshooting

Check logs for error categories and hints:
- **connection_refused**: Verify AMQP is running and port is accessible
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