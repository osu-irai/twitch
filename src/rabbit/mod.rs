pub mod types;

use eyre::{Context as _, bail};
use futures::StreamExt;
use hyper::client::conn;
use lapin::{
    options::{BasicAckOptions, BasicNackOptions},
    types::FieldTable,
    *,
};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tracing::debug_span;

use crate::{
    api::PostRequest,
    rabbit::types::{RequestContract, TwitchSettingsChangeContract},
};

// TODO: Move this to env
const AMQP_ADDRESS: &'static str = "amqp://irai:iraipass@localhost:5672";

pub async fn create_rmq_connection() -> eyre::Result<Connection> {
    match Connection::connect(AMQP_ADDRESS, ConnectionProperties::default()).await {
        Ok(conn) => {
            tracing::debug!(%AMQP_ADDRESS, "Created a RabbitMQ connection");
            Ok(conn)
        }
        Err(e) => {
            tracing::error!(?e, "Failed to create a RabbitMQ connection");
            Err(eyre::eyre!(
                "Failed to create a RabbitMQ connection, err: {e}"
            ))
        }
    }
}

pub async fn create_rmq_channel(conn: &Connection) -> eyre::Result<Channel> {
    let chan = conn
        .create_channel()
        .await
        .wrap_err("Failed to create a RabbitMQ channel")?;
    tracing::debug!(channel_id = chan.id(), "Created a RabbitMQ channel of ID");
    Ok(chan)
}

async fn create_rmq_queue(channel: &Channel, queue_name: &str) -> eyre::Result<Queue> {
    let queue = channel
        .queue_declare(
            queue_name,
            options::QueueDeclareOptions {
                durable: true,
                ..Default::default()
            },
            FieldTable::default(),
        )
        .await?;
    tracing::debug!(queue = %queue.name(), "Initialized RabbitMQ queue");
    Ok(queue)
}

pub async fn run_publish(
    channel: Channel,
    mut receiver: UnboundedReceiver<PostRequest>,
) -> eyre::Result<()> {
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
        match channel
            .basic_publish(
                "request-exchange",
                "",
                Default::default(),
                &serde_json::to_vec(&message)?,
                props.clone(),
            )
            .await
        {
            Ok(_) => {
                tracing::trace!("Successfully sent a message");
            }
            Err(err) => {
                bail!("Failed to send a message, {err:?}");
            }
        }
    }
    Ok(())
}

#[tracing::instrument]
pub async fn setup_twitch_settings_queue(connection: &Connection) -> eyre::Result<Consumer> {
    let channel = create_rmq_channel(&connection).await?;
    let queue = create_rmq_queue(&channel, "twitch-settings").await?;
    let consumer = channel
        .basic_consume(
            queue.name().as_str(),
            "twitch-settings-consumer",
            Default::default(),
            FieldTable::default(),
        )
        .await?;
    tracing::debug!(consumer_tag = %consumer.tag(), "Created a consumer");
    Ok(consumer)
}

#[tracing::instrument]
pub async fn run_twitch_queue(
    sender: UnboundedSender<TwitchSettingsChangeContract>,
    mut consumer: Consumer,
) -> eyre::Result<()> {
    while let Some(delivery) = consumer.next().await {
        match delivery {
            Ok(delivery) => {
                tracing::debug!(exchange = %delivery.exchange, "Received a message from the queue");
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
