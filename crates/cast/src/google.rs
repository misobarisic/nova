use crate::*;
use serde_json::{Value, json};
const MEDIA: &str = "urn:x-cast:com.google.cast.media";
const CONNECTION: &str = "urn:x-cast:com.google.cast.tp.connection";
pub(crate) struct Google {
    client: oxicast::CastClient,
    transport: Option<String>,
    app: Option<String>,
    pub occupied: bool,
    last_media: Option<i64>,
    track_ids: Vec<i64>,
}
impl Google {
    pub async fn connect(device: &Device) -> Result<Self> {
        // Cast receivers use protocol-defined self-signed TLS certificates. oxicast's
        // default trust policy is specific to this protocol; HTTPS verification is unaffected.
        let host = match device.address {
            std::net::SocketAddr::V6(a) if a.scope_id() != 0 => {
                format!("{}%{}", a.ip(), a.scope_id())
            }
            _ => device.address.ip().to_string(),
        };
        let client = oxicast::CastClient::builder(host, device.address.port())
            .auto_reconnect(false)
            .request_timeout(REQUEST_TIMEOUT)
            .connect()
            .await
            .map_err(cast_error)?;
        let mut google = Self {
            client,
            transport: None,
            app: None,
            occupied: false,
            last_media: None,
            track_ids: vec![],
        };
        google.refresh_app().await?;
        Ok(google)
    }
    pub(crate) async fn inspect_busy(&mut self) -> Result<()> {
        self.refresh_app().await
    }
    async fn refresh_app(&mut self) -> Result<()> {
        let status = self.client.receiver_status().await.map_err(cast_error)?;
        let current = status.applications.first();
        self.occupied = current.is_some();
        let transport = current.map(|a| a.transport_id.clone());
        if transport != self.transport {
            if let Some(t) = &transport {
                self.client
                    .send_raw_no_reply(CONNECTION, t, json!({"type":"CONNECT"}))
                    .await
                    .map_err(cast_error)?;
            }
            self.transport = transport;
        }
        self.app = current.map(|a| a.session_id.clone());
        Ok(())
    }
    async fn raw(&self, mut message: Value) -> Result<Value> {
        let t = self
            .transport
            .as_ref()
            .ok_or_else(|| error(CastErrorKind::Protocol))?;
        if let Some(id) = self.last_media
            && message.get("mediaSessionId").is_none()
            && !matches!(message["type"].as_str(), Some("GET_STATUS" | "LOAD"))
        {
            message["mediaSessionId"] = json!(id);
        }
        let response = self
            .client
            .send_raw(MEDIA, t, message)
            .await
            .map_err(cast_error)?;
        check_response(&response)?;
        Ok(response)
    }
    pub async fn snapshot(&mut self) -> Result<ReceiverSnapshot> {
        self.refresh_app().await?;
        let receiver = self.client.receiver_status().await.map_err(cast_error)?;
        let Some(_) = self.transport else {
            return Ok(ReceiverSnapshot {
                state: PlaybackState::Idle,
                volume: Some(receiver.volume.level),
                muted: Some(receiver.volume.muted),
                ..Default::default()
            });
        };
        let response = self.raw(json!({"type":"GET_STATUS"})).await?;
        let mut snapshot = parse_status(&response, self.app.clone())?;
        snapshot.volume = Some(receiver.volume.level);
        snapshot.muted = Some(receiver.volume.muted);
        self.last_media = snapshot.media_session.as_ref().and_then(|s| s.parse().ok());
        Ok(snapshot)
    }
    pub async fn load(&mut self, load: &MediaLoad) -> Result<ReceiverSnapshot> {
        let app = self
            .client
            .launch_app(&oxicast::CastApp::DefaultMediaReceiver)
            .await
            .map_err(cast_error)?;
        self.transport = Some(app.transport_id);
        self.app = Some(app.session_id);
        let tracks:Vec<Value>=load.text_tracks.iter().map(|t|json!({"trackId":t.id,"type":"TEXT","trackContentId":t.url,"trackContentType":t.mime_type,"name":t.name,"language":t.language,"subtype":"SUBTITLES"})).collect();
        let mut media = json!({"contentId":load.url,"contentType":load.mime_type,"streamType":"BUFFERED","metadata":{"metadataType":0,"title":load.title,"subtitle":load.subtitle},"tracks":tracks});
        if let Some(duration) = load.duration {
            media["duration"] = json!(duration);
        }
        let response=self.raw(json!({"type":"LOAD","sessionId":self.app,"media":media,"autoplay":!load.paused,"currentTime":load.start_position})).await?;
        let snapshot = parse_status(&response, self.app.clone())?;
        if snapshot.state == PlaybackState::Idle {
            return Err(error(CastErrorKind::Protocol));
        }
        self.last_media = snapshot.media_session.as_ref().and_then(|s| s.parse().ok());
        self.track_ids = load.text_tracks.iter().map(|t| t.id).collect();
        self.occupied = true;
        Ok(snapshot)
    }
    pub async fn command(&mut self, command: ReceiverCommand) -> Result<()> {
        match command {
            ReceiverCommand::SetVolume(v) => {
                self.client.set_volume(v).await.map_err(cast_error)?;
            }
            ReceiverCommand::SetMuted(m) => {
                self.client.set_muted(m).await.map_err(cast_error)?;
            }
            ReceiverCommand::SetPaused(paused) => {
                self.raw(json!({"type":if paused {"PAUSE"}else{"PLAY"}}))
                    .await?;
            }
            ReceiverCommand::Seek(position) => {
                self.raw(json!({"type":"SEEK","currentTime":position}))
                    .await?;
            }
            ReceiverCommand::SetTextTrack(id) => {
                if id.is_some_and(|id| !self.track_ids.contains(&id)) {
                    return Err(error(CastErrorKind::InvalidInput));
                }
                self.raw(json!({"type":"EDIT_TRACKS_INFO","activeTrackIds":id.into_iter().collect::<Vec<_>>()})).await?;
            }
        }
        Ok(())
    }
    pub async fn reconnect(&mut self) -> Result<()> {
        self.client
            .reconnect()
            .await
            .map_err(|_| error(CastErrorKind::Network))
    }
    pub async fn stop(&mut self) -> Result<()> {
        self.raw(json!({"type":"STOP"})).await?;
        Ok(())
    }
}
fn check_response(value: &Value) -> Result<()> {
    if matches!(
        value["type"].as_str(),
        Some("INVALID_REQUEST" | "LOAD_FAILED" | "LOAD_CANCELLED" | "INVALID_PLAYER_STATE")
    ) {
        Err(error(CastErrorKind::Protocol))
    } else {
        Ok(())
    }
}
fn parse_status(value: &Value, app: Option<String>) -> Result<ReceiverSnapshot> {
    check_response(value)?;
    let entries = value["status"]
        .as_array()
        .ok_or_else(|| error(CastErrorKind::Protocol))?;
    let Some(s) = entries.first() else {
        return Ok(ReceiverSnapshot {
            app_session: app,
            state: PlaybackState::Idle,
            ..Default::default()
        });
    };
    let commands = s["supportedMediaCommands"].as_u64().unwrap_or(0);
    let media = &s["media"];
    Ok(ReceiverSnapshot {
        app_session: app,
        media_session: s["mediaSessionId"].as_i64().map(|id| id.to_string()),
        content_id: media["contentId"].as_str().map(str::to_owned),
        state: match s["playerState"].as_str() {
            Some("PLAYING") => PlaybackState::Playing,
            Some("PAUSED") => PlaybackState::Paused,
            Some("BUFFERING" | "LOADING") => PlaybackState::Buffering,
            Some("IDLE") => PlaybackState::Idle,
            _ => PlaybackState::Unknown,
        },
        position: s["currentTime"]
            .as_f64()
            .filter(|v| v.is_finite() && *v >= 0.)
            .unwrap_or(0.),
        duration: media["duration"]
            .as_f64()
            .filter(|v| v.is_finite() && *v >= 0.),
        volume: None,
        muted: None,
        capabilities: ReceiverCapabilities {
            pause: commands & 1 != 0,
            seek: commands & 2 != 0,
            volume: true,
            mute: true,
            text_tracks: media["tracks"]
                .as_array()
                .is_some_and(|ts| ts.iter().any(|t| t["type"] == "TEXT")),
            ..Default::default()
        },
        end_reason: match s["idleReason"].as_str() {
            Some("FINISHED") => Some(EndReason::Finished),
            Some("CANCELLED") => Some(EndReason::Cancelled),
            Some("INTERRUPTED") => Some(EndReason::Interrupted),
            Some("ERROR") => Some(EndReason::Error),
            _ => None,
        },
        ..Default::default()
    })
}
fn cast_error(e: oxicast::Error) -> CastError {
    error(match e {
        oxicast::Error::Timeout(_) => CastErrorKind::Timeout,
        oxicast::Error::Connect(_) | oxicast::Error::Tls(_) | oxicast::Error::Disconnected => {
            CastErrorKind::Network
        }
        _ => CastErrorKind::Protocol,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn status_caps_are_advertised_and_identity_is_retained() {
        let value = json!({"status":[{"mediaSessionId":7,"playerState":"PLAYING","currentTime":12,"supportedMediaCommands":1,"media":{"contentId":"http://host/token","duration":90}}]});
        let s = parse_status(&value, Some("app".into())).unwrap();
        assert!(s.capabilities.pause);
        assert!(!s.capabilities.seek);
        assert_eq!(s.media_session.as_deref(), Some("7"));
        assert_eq!(s.position, 12.);
    }
    #[test]
    fn cast_errors_and_malformed_status_rejected() {
        assert!(parse_status(&json!({"type":"LOAD_FAILED","reason":"secret"}), None).is_err());
        assert!(parse_status(&json!({}), None).is_err());
    }
}
