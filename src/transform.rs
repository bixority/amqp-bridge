use anyhow::Result;
use async_trait::async_trait;
use lapin::BasicProperties;
use std::borrow::Cow;

pub struct Message<'a> {
    pub data: Cow<'a, [u8]>,
    pub properties: BasicProperties,
}

#[async_trait]
pub trait MessageTransformer: Send + Sync {
    /// Transform an incoming AMQP `Message` into a new `Message` before publish.
    ///
    /// # Errors
    /// Implementations may return an error when transformation fails, e.g.
    /// invalid payloads, schema violations, or other application-specific
    /// conditions that prevent producing an output message.
    async fn transform<'a>(&self, input: Message<'a>) -> Result<Message<'a>>;
}
