pub mod types;

use eyre::{Context as _, bail};
use futures::StreamExt;
use lapin::{
    options::{BasicAckOptions, BasicNackOptions},
    types::FieldTable,
    *,
};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::{api::PostRequest, rabbit::types::TwitchSettingsChangeContract};

// TODO: Move this to env
const AMQP_ADDRESS: &'static str = "amqp://irai:iraipass@localhost:5672";

/// Create a base RMQ connection to use for future channels
pub async fn create_rmq_connection() -> eyre::Result<Connection> {
    Connection::connect(AMQP_ADDRESS, ConnectionProperties::default())
        .await
        .inspect(|_| tracing::debug!(%AMQP_ADDRESS, "Created a RabbitMQ connection"))
        .inspect_err(|e| tracing::error!(?e, "Failed to create a RabbitMQ connection"))
        .wrap_err("Failed to create a RabbitMQ connection")
}

pub async fn create_rmq_channel(conn: &Connection) -> eyre::Result<Channel> {
    conn.create_channel()
        .await
        .inspect(|v| tracing::debug!(channel_id = v.id(), "Created a RabbitMQ channel"))
        .wrap_err("Failed to create a RabbitMQ channel")
}

async fn create_rmq_queue(channel: &Channel, queue_name: &str) -> eyre::Result<Queue> {
    channel
        .queue_declare(
            queue_name,
            options::QueueDeclareOptions {
                durable: true,
                ..Default::default()
            },
            FieldTable::default(),
        )
        .await
        .inspect(|queue| tracing::debug!(queue = %queue.name(), "Initialized RabbitMQ queue"))
        .wrap_err("Failed to create a queue")
}

pub async fn run_publish(
    channel: Channel,
    mut receiver: UnboundedReceiver<PostRequest>,
) -> eyre::Result<()> {
    // MassTransit likes to complain about type mismatches on ser/de without an explicit header
    let mut headers = FieldTable::default();
    headers.insert(
        "MT-MessageType".into(),
        lapin::types::AMQPValue::LongString(
            "urn:message:osuRequestor.DTO.Requests:PostBaseRequest".into(),
        ),
    );

    let props = BasicProperties::default()
        .with_content_type("application/json".into())
        .with_headers(headers);

    while let Some(message) = receiver.recv().await {
        channel
            .basic_publish(
                "request-exchange",
                "",
                Default::default(),
                &serde_json::to_vec(&message)?,
                props.clone(),
            )
            .await
            .inspect(|_| tracing::trace!("Successfully sent a message"))
            .wrap_err("Failed to send a message")?;
    }
    Ok(())
}

#[tracing::instrument]
pub async fn setup_twitch_settings_queue(connection: &Connection) -> eyre::Result<Consumer> {
    let channel = create_rmq_channel(&connection).await?;
    let queue = create_rmq_queue(&channel, "twitch-settings").await?;

    channel
        .basic_consume(
            queue.name().as_str(),
            "twitch-settings-consumer",
            Default::default(),
            FieldTable::default(),
        )
        .await
        .inspect(|consumer| tracing::debug!(consumer_tag = %consumer.tag(), "Created a consumer"))
        .wrap_err("Failed to create a consumer")
}

#[tracing::instrument]
pub async fn run_twitch_queue(
    sender: UnboundedSender<TwitchSettingsChangeContract>,
    mut consumer: Consumer,
) -> eyre::Result<()> {
    while let Some(delivery) = consumer.next().await {
        match delivery {
            Ok(delivery) => {
                tracing::trace!(exchange = %delivery.exchange, "Received a message from the queue");
                let payload = String::from_utf8_lossy(&delivery.data);

                match serde_json::from_str::<TwitchSettingsChangeContract>(&payload) {
                    Ok(settings) => {
                        tracing::debug!(message_id = delivery.delivery_tag, message = ?settings, "Message is valid");
                        // Acknowledge the message
                        delivery
                            .ack(BasicAckOptions::default())
                            .await
                            .expect("Failed to ack");

                        tracing::debug!(message_id = delivery.delivery_tag, "Message acknowledged");
                        sender.send(settings)?;
                    }
                    Err(e) => {
                        tracing::warn!(
                            message_id = delivery.delivery_tag,
                            ?e,
                            "Failed to deserialize message, nacking"
                        );

                        // Reject and don't requeue the message (dead letter it or discard)
                        delivery
                            .nack(BasicNackOptions {
                                requeue: false,
                                ..Default::default()
                            })
                            .await
                            .expect("Failed to nack");
                    }
                }
            }
            Err(e) => {
                tracing::error!(?e, "Failed to receive message");
                break;
            }
        }
    }
    Ok(())
}
