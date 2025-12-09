use futures::StreamExt;
use lapin::{
    options::{BasicAckOptions, BasicNackOptions},
    types::FieldTable,
    *,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
struct MassTransitMessage<T> {
    #[serde(rename = "message")]
    message: T,
}

#[derive(Debug, Serialize, Deserialize)]
struct TwitchSettingsChangeContract {
    #[serde(rename = "twitchUserId")]
    user_id: String,
    #[serde(rename = "isEnabled")]
    is_enabled: bool,
}

pub async fn setup_rmq() -> eyre::Result<()> {
    let addr = "amqp://irai:iraipass@localhost:5672";
    let conn = match Connection::connect(addr, ConnectionProperties::default()).await {
        Ok(conn) => {
            println!("Success");
            conn
        }
        Err(e) => {
            println!("Fucked up, error: {e:?}");
            return Err(eyre::eyre!("Fucked up, err: {e}"));
        }
    };

    let channel = conn.create_channel().await?;

    let queue = channel
        .queue_declare(
            "twitch-settings",
            options::QueueDeclareOptions {
                durable: true,
                ..Default::default()
            },
            FieldTable::default(),
        )
        .await?;
    println!("Initialized queue and channel");

    let mut consumer = channel
        .basic_consume(
            "twitch-settings",
            "twitch-settings-consumer",
            Default::default(),
            FieldTable::default(),
        )
        .await?;

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
