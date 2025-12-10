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

use crate::rabbit::types::{RequestContract, TwitchSettingsChangeContract};

// TODO: Move this to env
const AMQP_ADDRESS: &'static str = "amqp://irai:iraipass@localhost:5672";

pub async fn create_rmq_connection() -> eyre::Result<Connection> {
    match Connection::connect(AMQP_ADDRESS, ConnectionProperties::default()).await {
        Ok(conn) => {
            println!("Success");
            Ok(conn)
        }
        Err(e) => {
            println!("Fucked up, error: {e:?}");
            Err(eyre::eyre!("Fucked up, err: {e}"))
        }
    }
}

async fn create_rmq_channel(conn: &Connection) -> eyre::Result<Channel> {
    conn.create_channel()
        .await
        .wrap_err("Failed to create a RabbitMQ channel")
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
    println!("Initialized queue and channel");
    Ok(queue)
}

pub async fn run_publish(
    channel: Channel,
    mut receiver: UnboundedReceiver<RequestContract>,
) -> eyre::Result<()> {
    while let Some(message) = receiver.recv().await {
        match channel
            .basic_publish(
                "request-exchange",
                "",
                Default::default(),
                &serde_json::to_vec(&message)?,
                Default::default(),
            )
            .await
        {
            Ok(_) => {}
            Err(err) => {
                bail!("Failed to send a message, {err:?}");
            }
        }
    }
    Ok(())
}

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
    Ok(consumer)
}

pub async fn run_twitch_queue(
    sender: UnboundedSender<TwitchSettingsChangeContract>,
    mut consumer: Consumer,
) -> eyre::Result<()> {
    while let Some(delivery) = consumer.next().await {
        match delivery {
            Ok(delivery) => {
                let payload = String::from_utf8_lossy(&delivery.data);

                match serde_json::from_str::<TwitchSettingsChangeContract>(&payload) {
                    Ok(settings) => {
                        println!("Received settings change: {settings:#?}");
                        // Acknowledge the message
                        delivery
                            .ack(BasicAckOptions::default())
                            .await
                            .expect("Failed to ack");

                        println!("Message acknowledged");
                        sender.send(settings)?;
                    }
                    Err(e) => {
                        eprintln!("\nFailed to deserialize message: {}", e);
                        eprintln!("Raw payload: {}", payload);

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
                eprintln!("Error receiving message: {}", e);
                break;
            }
        }
    }
    Ok(())
}
