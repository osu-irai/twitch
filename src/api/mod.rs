use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct TwitchDTO {
    pub user_id: String,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct PostRequest {
    pub destination_id: u32,
    pub beatmap_id: u32,
}

pub fn get_twitch_users() -> eyre::Result<Vec<TwitchDTO>> {
    todo!()
}
