use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct TwitchDTO {
    pub user_id: String,
}

pub fn get_twitch_users() -> eyre::Result<Vec<TwitchDTO>> {
    todo!()
}
