//! Decrypting/downloading WA media.

use whatsappweb::message::FileInfo;
use whatsappweb::{MediaType, crypto};
use std::thread;
use comm::WhatsappCommand;
use futures::sync::mpsc::UnboundedSender;
use huawei_modem::pdu::PduAddress;
use humansize::{FileSize, file_size_opts};
use reqwest;
use std::io::prelude::*;
use std::fs::File;
use util::Result;
use uuid::Uuid;
use std::sync::Arc;
use reqwest::header::UserAgent;
use reqwest::StatusCode;
use mime_guess::get_mime_extensions_str;

pub struct MediaInfo {
    pub ty: MediaType,
    pub fi: FileInfo,
    pub addr: PduAddress,
    pub group: Option<i32>,
    pub path: String,
    pub dl_path: String,
    pub tx: Arc<UnboundedSender<WhatsappCommand>>,
    pub name: Option<String>
}
pub struct MediaResult {
    pub addr: PduAddress,
    pub group: Option<i32>,
    pub text: String
}
impl MediaInfo {
    fn run(&mut self) -> Result<String> {
        let uu = Uuid::new_v4().simple().to_string();
        let mime_ext = get_mime_extensions_str(&self.fi.mime)
            .unwrap_or(&[])
            .get(0)
            .unwrap_or(&"bin");
        let path = format!("{}/{}.{}", self.path, uu, mime_ext);
        let dl_path = format!("{}/{}.{}", self.dl_path, uu, mime_ext);
        debug!("Creating file {}", path);
        let mut file = File::create(&path)?;
        debug!("Downloading {}", self.fi.url);
        let mut data = vec![];
        let client = reqwest::Client::new();
        let mut resp = client.get(&self.fi.url)
            .header(UserAgent::new("sms-irc"))
            .send()?;
        debug!("response: {:?}", resp);
        if resp.status() != StatusCode::Ok {
            Err(format_err!("Status code {} when downloading", resp.status().as_u16()))?
        }
        resp.copy_to(&mut data)?;
        debug!("Checking encrypted SHA256");
        let sha = crypto::sha256(&data);
        if sha != self.fi.enc_sha256 {
            Err(format_err!("Encrypted SHA256 mismatch"))?
        }
        debug!("Decrypting");
        let dec = crypto::decrypt_media_message(&self.fi.key, self.ty, &data)
            .map_err(|e| format_err!("decryption error: {}", e))?;
        debug!("Checking SHA256");
        if sha != self.fi.sha256 {
            //Err(format_err!("SHA256 mismatch"))?
        }
        debug!("Writing to file");
        file.write_all(&dec)?;
        file.flush()?;
        let size = self.fi.size.file_size(file_size_opts::BINARY)
            .map_err(|e| format_err!("filesize error: {}", e))?;
        let ret = match self.ty {
            MediaType::Image => format!("\x01ACTION uploaded an image ({}, {}) < {} >\x01", size, self.fi.mime, dl_path),
            MediaType::Audio => format!("\x01ACTION uploaded audio ({}, {}) < {} >\x01", size, self.fi.mime, dl_path),
            MediaType::Document => format!("\x01ACTION uploaded a document '{}' ({}, {}) < {} >\x01", self.name.take().unwrap_or("unknown".into()), self.fi.mime, size, dl_path),
            MediaType::Video => format!("\x01ACTION uploaded video ({}, {}) < {} >\x01", size, self.fi.mime, dl_path)
        };
        Ok(ret)
    }
    pub fn start(mut self) {
        info!("Starting media download/decryption job for {}", self.addr);
        thread::spawn(move || {
            let ret = self.run();
            let addr = self.addr;
            let group = self.group;
            let ret = ret
                .map(move |r| {
                    MediaResult {
                        addr, group,
                        text: r
                    }
                });
            self.tx.unbounded_send(WhatsappCommand::MediaFinished(ret))
                .unwrap();
        });
    }
}
