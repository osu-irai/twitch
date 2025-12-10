use serde::{Deserialize, Serialize};

// pub enum MessageType {
//     TwitchSettings(TwitchSettingsChangeContract),
//     Request(RequestContract),
// }

/// Message, signifying that user toggled their Twitch integration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TwitchSettingsChangeContract {
    /// Twitch user ID
    #[serde(rename = "twitchUserId")]
    pub user_id: String,
    /// Current toggle of the integration.
    /// There might be cases when the message has the same value
    /// as the in-memory user
    #[serde(rename = "isEnabled")]
    pub is_enabled: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestContract {
    /// Target username of the request
    /// Should probably be changed to user ID,
    /// but I am reusing an existing contract
    pub target: String,
    /// Request metadata. Bad type name due to reuse
    /// of an existing frontend DTO
    pub request: ReceivedRequestResponse,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceivedRequestResponse {
    /// Request ID. Ignored on sent requests,
    /// necessary for received requests
    pub id: u64,
    /// Beatmap metadata
    pub beatmap: BeatmapDTO,
    /// User who created the request. Is `None` if the request
    /// is made from the bot, might be nice if the Bot ever returns
    /// received requests as well
    pub from: Option<UserDTO>,
    /// Which service the request comes from. Always `Twitch` from the bot
    pub source: RequestSource,
}

#[derive(Debug, Serialize, Deserialize)]
enum RequestSource {
    Website,
    Twitch,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeatmapDTO {
    pub beatmap_id: usize,
    pub beatmapset_id: usize,
    pub artist: String,
    pub title: String,
    pub difficulty: String,
    pub stars: f64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserDTO {
    pub id: u64,
    pub username: String,
    pub avatar_url: String,
}
