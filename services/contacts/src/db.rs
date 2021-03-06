/// DB interface for the Contacts
use crate::generated::common::*;
use common::traits::DispatcherId;
use common::SystemTime;
use log::{debug, error};
use rusqlite::{Connection, Row, Statement, NO_PARAMS};
use sqlite_utils::{DatabaseUpgrader, SqliteDb};
use std::io::BufReader;
use std::sync::mpsc::{channel, Sender};
use std::time::{Duration, UNIX_EPOCH};
use thiserror::Error;
use uuid::Uuid;

#[cfg(not(target_os = "android"))]
const DB_PATH: &str = "./contacts.sqlite";
#[cfg(target_os = "android")]
const DB_PATH: &str = "/data/local/service/api-daemon/contacts.sqlite";

const MIN_MATCH_DIGITS: usize = 7;

#[derive(Error, Debug)]
pub enum Error {
    #[error("SQlite error")]
    Sqlite(#[from] rusqlite::Error),
    #[error("Serde JSON error")]
    Json(#[from] serde_json::Error),
    #[error("Invalid FilterOption error")]
    InvalidFilterOption(String),
    #[error("Invalid contact id error")]
    InvalidContactId(String),
    #[error("Ice position already used")]
    IcePositionUsed(String),
}

pub struct ContactsSchemaManager {}

static UPGRADE_0_1_SQL: [&str; 12] = [
    // Main table holding main data of contact.
    r#"CREATE TABLE IF NOT EXISTS contact_main (
        contact_id   TEXT    NOT NULL PRIMARY KEY,
        name         TEXT    DEFAULT (''),
        family_name  TEXT    DEFAULT (''),
        given_name   TEXT    DEFAULT (''),
        tel_number   TEXT    DEFAULT (''),
        tel_json     TEXT    DEFAULT (''),
        email        TEXT    DEFAULT (''),
        email_json   TEXT    DEFAULT (''),
        photo_type   TEXT    DEFAULT (''),
        photo_blob   BLOB    DEFAULT (x''),
        published    INTEGER DEFAULT (0),
        updated      INTEGER DEFAULT (0),
        bday         INTEGER DEFAULT (0),
        anniversary  INTEGER DEFAULT (0)
    )"#,
    r#"CREATE INDEX idx_name ON contact_main(name)"#,
    r#"CREATE INDEX idx_famil_name ON contact_main(family_name)"#,
    r#"CREATE INDEX idx_given_name ON contact_main(given_name)"#,
    r#"CREATE INDEX idx_tel_number ON contact_main(tel_number)"#,
    r#"CREATE INDEX idx_email ON contact_main(email)"#,
    r#"CREATE TABLE IF NOT EXISTS contact_additional (
        contact_id TEXT NOT NULL,
        data_type TEXT NOT NULL,
        value TEXT DEFAULT '',
        FOREIGN KEY(contact_id) REFERENCES contact_main(contact_id)
    )"#,
    r#"CREATE INDEX idx_additional ON contact_additional(contact_id)"#,
    r#"CREATE TABLE IF NOT EXISTS blocked_numbers (number TEXT NOT NULL UNIQUE)"#,
    r#"CREATE TABLE IF NOT EXISTS speed_dials (
        dial_key TEXT NOT NULL UNIQUE, 
        tel TEXT NOT NULL, 
        contact_id TEXT
    )"#,
    r#"CREATE TABLE IF NOT EXISTS groups (id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE)"#,
    r#"CREATE TABLE IF NOT EXISTS group_contacts (
       id INTEGER PRIMARY KEY ASC, 
       group_id TEXT NOT NULL, 
       contact_id TEXT NOT NULL,
       FOREIGN KEY(group_id) REFERENCES groups(id),
       FOREIGN KEY(contact_id) REFERENCES contact_main(contact_id)
    )"#,
];

impl DatabaseUpgrader for ContactsSchemaManager {
    fn upgrade(&mut self, from: u32, to: u32, connection: &Connection) -> bool {
        // We only support version 1 currently.
        if !(from == 0 && to == 1) {
            return false;
        }

        for cmd in &UPGRADE_0_1_SQL {
            if let Err(err) = connection.execute(cmd, NO_PARAMS) {
                error!("Upgrade step failure: {}", err);
                return false;
            }
        }

        true
    }
}

fn row_to_contact_id(row: &Row) -> Result<String, Error> {
    let column = row.column_index("contact_id")?;
    Ok(row.get(column)?)
}

impl Into<String> for SortOption {
    fn into(self) -> String {
        match self {
            Self::GivenName => "given_name".to_string(),
            Self::FamilyName => "family_name".to_string(),
            Self::Name => "name".to_string(),
        }
    }
}

impl Into<String> for Order {
    fn into(self) -> String {
        match self {
            Self::Ascending => "ASC".to_string(),
            Self::Descending => "DESC".to_string(),
        }
    }
}

#[derive(Debug)]
struct MainRowData {
    contact_id: String,
    name: String,
    family_name: String,
    given_name: String,
    tel_json: String,
    email_json: String,
    photo_type: String,
    photo_blob: Vec<u8>,
    published: i64,
    updated: i64,
    bday: i64,
    anniversary: i64,
}

#[derive(Debug)]
struct AdditionalRowData {
    contact_id: String,
    data_type: String,
    value: String,
}

impl Default for ContactInfo {
    fn default() -> Self {
        ContactInfo {
            id: "".to_string(),
            published: SystemTime::from(UNIX_EPOCH),
            updated: SystemTime::from(UNIX_EPOCH),
            bday: SystemTime::from(UNIX_EPOCH),
            anniversary: SystemTime::from(UNIX_EPOCH),
            sex: "".to_string(),
            gender_identity: "".to_string(),
            ringtone: "".to_string(),
            photo_type: "".to_string(),
            photo_blob: vec![],
            addresses: None,
            email: None,
            url: None,
            name: "".to_string(),
            tel: None,
            honorific_prefix: None,
            given_name: "".to_string(),
            phonetic_given_name: "".to_string(),
            additional_name: None,
            family_name: "".to_string(),
            phonetic_family_name: "".to_string(),
            honorific_suffix: None,
            nickname: None,
            category: None,
            org: None,
            job_title: None,
            note: None,
            groups: None,
            ice_position: 0,
        }
    }
}

impl From<&SimContactInfo> for ContactInfo {
    fn from(sim_contact_info: &SimContactInfo) -> Self {
        let mut contact = ContactInfo::default();

        contact.id = sim_contact_info.id.to_string();
        contact.name = sim_contact_info.name.to_string();
        contact.family_name = sim_contact_info.name.to_string();
        contact.given_name = sim_contact_info.name.to_string();

        let sim_tels: Vec<&str> = sim_contact_info.tel.split('\u{001E}').collect();
        let tels = sim_tels
            .iter()
            .map(|x| ContactTelField {
                atype: "".to_string(),
                value: x.to_string(),
                pref: false,
                carrier: "".to_string(),
            })
            .collect();
        contact.tel = Some(tels);

        let sim_emails: Vec<&str> = sim_contact_info.email.split('\u{001E}').collect();
        let emails = sim_emails
            .iter()
            .map(|x| ContactField {
                atype: "".to_string(),
                value: x.to_string(),
                pref: false,
            })
            .collect();
        contact.email = Some(emails);

        contact.category = Some(vec!["SIM".to_owned()]);

        contact.published = SystemTime::from(std::time::SystemTime::now());
        contact.updated = SystemTime::from(std::time::SystemTime::now());

        contact
    }
}

macro_rules! fillVecField {
    ($field: expr, $value: expr) => {
        if $field.is_none() {
            $field = Some(vec![$value]);
        } else {
            if let Some(fields) = $field.as_mut() {
                fields.push($value);
            }
        }
    };
}

macro_rules! saveVecField {
    ($stmt: expr, $id: expr, $type: expr, $datas: expr) => {
        if let Some(values) = &$datas {
            for value in values {
                $stmt.insert(&[&$id as &dyn rusqlite::ToSql, &$type.to_string(), &value])?;
            }
        }
    };
}

macro_rules! saveStrField {
    ($stmt: expr, $id: expr, $type: expr, $data: expr) => {
        if !$data.is_empty() {
            $stmt.insert(&[&$id as &dyn rusqlite::ToSql, &$type.to_string(), &$data])?;
        }
    };
}

impl ContactInfo {
    fn fill_main_data(&mut self, id: &str, conn: &Connection) -> Result<(), Error> {
        self.id = id.into();
        let mut stmt = conn.prepare("SELECT contact_id, name, family_name, given_name, tel_json, email_json,
        photo_type, photo_blob, published, updated, bday, anniversary FROM contact_main WHERE contact_id=:id")?;

        let rows = stmt.query_map_named(&[(":id", &self.id)], |row| {
            Ok(MainRowData {
                contact_id: row.get(0)?,
                name: row.get(1)?,
                family_name: row.get(2)?,
                given_name: row.get(3)?,
                tel_json: row.get(4)?,
                email_json: row.get(5)?,
                photo_type: row.get(6)?,
                photo_blob: row.get(7)?,
                published: row.get(8)?,
                updated: row.get(9)?,
                bday: row.get(10)?,
                anniversary: row.get(11)?,
            })
        })?;

        for result_row in rows {
            let row = result_row?;
            debug!("Current row data is {:#?}", row);
            self.name = row.name;
            self.family_name = row.family_name;
            self.given_name = row.given_name;

            if !row.tel_json.is_empty() {
                let tel: Vec<ContactTelField> = serde_json::from_str(&row.tel_json)?;
                self.tel = Some(tel);
            }

            if !row.email_json.is_empty() {
                let email: Vec<ContactField> = serde_json::from_str(&row.email_json)?;
                self.email = Some(email);
            }

            self.photo_type = row.photo_type;
            self.photo_blob = row.photo_blob;

            if let Some(time) = UNIX_EPOCH.checked_add(Duration::from_secs(row.published as u64)) {
                self.published = SystemTime::from(time);
            }

            if let Some(time) = UNIX_EPOCH.checked_add(Duration::from_secs(row.updated as u64)) {
                self.updated = SystemTime::from(time);
            }

            if let Some(time) = UNIX_EPOCH.checked_add(Duration::from_secs(row.bday as u64)) {
                self.bday = SystemTime::from(time);
            }

            if let Some(time) = UNIX_EPOCH.checked_add(Duration::from_secs(row.anniversary as u64))
            {
                self.anniversary = SystemTime::from(time);
            }
        }
        Ok(())
    }

    fn fill_additional_data(&mut self, id: &str, conn: &Connection) -> Result<(), Error> {
        self.id = id.into();
        let mut stmt = conn.prepare(
            "SELECT contact_id, data_type, value FROM contact_additional WHERE contact_id=:id",
        )?;
        let rows = stmt.query_map_named(&[(":id", &id)], |row| {
            Ok(AdditionalRowData {
                contact_id: row.get(0)?,
                data_type: row.get(1)?,
                value: row.get(2)?,
            })
        })?;

        for result_row in rows {
            let row = result_row?;
            if row.data_type == "honorific_prefix" {
                fillVecField!(self.honorific_prefix, row.value);
            } else if row.data_type == "phonetic_given_name" {
                self.phonetic_given_name = row.value;
            } else if row.data_type == "phonetic_family_name" {
                self.phonetic_family_name = row.value;
            } else if row.data_type == "additional_name" {
                fillVecField!(self.additional_name, row.value);
            } else if row.data_type == "honorific_suffix" {
                fillVecField!(self.honorific_suffix, row.value);
            } else if row.data_type == "nickname" {
                fillVecField!(self.nickname, row.value);
            } else if row.data_type == "category" {
                fillVecField!(self.category, row.value);
            } else if row.data_type == "org" {
                fillVecField!(self.org, row.value);
            } else if row.data_type == "job_title" {
                fillVecField!(self.job_title, row.value);
            } else if row.data_type == "note" {
                fillVecField!(self.note, row.value);
            } else if row.data_type == "addresses" {
                if !&row.value.is_empty() {
                    let addr: Vec<Address> = serde_json::from_str(&row.value)?;
                    self.addresses = Some(addr);
                }
            } else if row.data_type == "ringtone" {
                self.ringtone = row.value;
            } else if row.data_type == "gender_identity" {
                self.gender_identity = row.value;
            } else if row.data_type == "sex" {
                self.sex = row.value;
            } else if row.data_type == "url" {
                if !row.value.is_empty() {
                    let url: Vec<ContactField> = serde_json::from_str(&row.value)?;
                    self.url = Some(url);
                }
            } else if row.data_type == "groups" {
                fillVecField!(self.groups, row.value);
            } else if row.data_type == "ice_position" {
                self.ice_position = row.value.parse().unwrap_or(0);
            } else {
                error!("Unknown type in addtional :{}", row.data_type);
            }
        }
        Ok(())
    }

    fn save_main_data(&self, tx: &rusqlite::Transaction) -> Result<(), Error> {
        let mut stmt_ins = tx.prepare("INSERT INTO contact_main (contact_id, name, family_name, given_name, 
            tel_number, tel_json, email, email_json, photo_type, photo_blob, published, updated, bday, 
            anniversary) VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)")?;

        let mut tel_number: String = "\u{001E}".to_string();
        let mut tel_json = "".to_string();
        if let Some(tels) = &self.tel {
            for tel in tels {
                tel_number += &tel.value;
                // Seperate with unprintable character, used for find.
                tel_number.push_str("\u{001E}");
            }
            // The tel_json is used for restore the tel struct.
            tel_json = serde_json::to_string(tels)?;
        }

        let mut email_address = "\u{001E}".to_string();
        let mut email_json = "".to_string();
        if let Some(emails) = &self.email {
            for email in emails {
                email_address += &email.value;
                // Seperate with unprintable character, used for find.
                email_address.push_str("\u{001E}");
            }
            // The email_json is used for restore the email struct.
            email_json = serde_json::to_string(emails)?;
        }

        let mut published = 0;
        if let Ok(duration) = self.published.duration_since(UNIX_EPOCH) {
            published = duration.as_secs() as i64;
        }

        let mut updated = 0;
        if let Ok(duration) = self.updated.duration_since(UNIX_EPOCH) {
            updated = duration.as_secs() as i64;
        }

        let mut bday = 0;
        if let Ok(duration) = self.bday.duration_since(UNIX_EPOCH) {
            bday = duration.as_secs() as i64;
        }

        let mut anniversary = 0;
        if let Ok(duration) = self.anniversary.duration_since(UNIX_EPOCH) {
            anniversary = duration.as_secs() as i64;
        }

        stmt_ins.insert(&[
            &self.id as &dyn rusqlite::ToSql,
            &self.name,
            &self.family_name,
            &self.given_name,
            &tel_number,
            &tel_json,
            &email_address,
            &email_json,
            &self.photo_type,
            &self.photo_blob,
            &published,
            &updated,
            &bday,
            &anniversary,
        ])?;
        Ok(())
    }

    fn save_additional_data(&self, tx: &rusqlite::Transaction) -> Result<(), Error> {
        let mut stmt = tx.prepare(
            "INSERT INTO contact_additional (contact_id, data_type, value) VALUES(?, ?, ?)",
        )?;
        saveVecField!(stmt, self.id, "honorific_prefix", self.honorific_prefix);
        saveVecField!(stmt, self.id, "additional_name", self.additional_name);
        saveVecField!(stmt, self.id, "honorific_suffix", self.honorific_suffix);
        saveVecField!(stmt, self.id, "nickname", self.nickname);
        saveVecField!(stmt, self.id, "category", self.category);
        saveVecField!(stmt, self.id, "org", self.org);
        saveVecField!(stmt, self.id, "job_title", self.job_title);
        saveVecField!(stmt, self.id, "note", self.note);
        saveStrField!(stmt, self.id, "sex", self.sex);
        saveStrField!(stmt, self.id, "gender_identity", self.gender_identity);
        saveStrField!(stmt, self.id, "ringtone", self.ringtone);
        saveStrField!(
            stmt,
            self.id,
            "phonetic_given_name",
            self.phonetic_given_name
        );
        saveStrField!(
            stmt,
            self.id,
            "phonetic_family_name",
            self.phonetic_family_name
        );

        if self.ice_position != 0 {
            saveStrField!(stmt, self.id, "ice_position", self.ice_position.to_string());
        }

        if let Some(addresses) = &self.addresses {
            let json = serde_json::to_string(addresses)?;
            saveStrField!(stmt, self.id, "addresses", json);
        }

        if let Some(url) = &self.url {
            let json = serde_json::to_string(url)?;
            saveStrField!(stmt, self.id, "url", json);
        }

        if let Some(groups) = &self.groups {
            for group in groups {
                stmt.insert(&[&self.id, "groups", &group])?;
                // Update the group_contacts when contact with group info.
                let mut stmt_group =
                    tx.prepare("INSERT INTO group_contacts (group_id, contact_id) VALUES(?, ?)")?;
                stmt_group.insert(&[&group, &self.id])?;
            }
        }

        Ok(())
    }
}

// Creates a contacts database with a proper updater.
// SQlite manages itself thread safety so we can use
// multiple ones without having to use Rust mutexes.
fn create_db() -> SqliteDb {
    let db = match SqliteDb::open(DB_PATH, &mut ContactsSchemaManager {}, 1) {
        Ok(db) => db,
        Err(err) => panic!("Failed to open contacts db: {}", err),
    };
    if let Err(err) = db.enable_wal() {
        error!("Failed to enable WAL mode on contacts db: {}", err);
    }

    db
}

enum CursorCommand {
    Next(Sender<Option<Vec<ContactInfo>>>),
    Stop,
}

pub struct ContactDbCursor {
    sender: Sender<CursorCommand>,
}

impl Iterator for ContactDbCursor {
    type Item = Vec<ContactInfo>;

    fn next(&mut self) -> Option<Self::Item> {
        let (sender, receiver) = channel();

        let _ = self.sender.send(CursorCommand::Next(sender));
        match receiver.recv() {
            Ok(msg) => msg,
            Err(err) => {
                error!("Failed to receive cursor response: {}", err);
                None
            }
        }
    }
}

impl Drop for ContactDbCursor {
    fn drop(&mut self) {
        debug!("ContactDbCursor::drop");
        let _ = self.sender.send(CursorCommand::Stop);
    }
}

impl ContactDbCursor {
    pub fn new<F: 'static>(batch_size: i64, only_main_data: bool, prepare: F) -> Self
    where
        F: Fn(&Connection) -> Option<Statement> + Send,
    {
        let (sender, receiver) = channel();
        let _ = std::thread::spawn(move || {
            let mut db = create_db();
            let connection = db.mut_connection();
            let mut statement = match prepare(&connection) {
                Some(statement) => statement,
                None => {
                    // Return an empty cursor.
                    // We use a trick here where we create a request that will always return 0 results.
                    connection
                        .prepare("SELECT contact_id FROM contact_main where contact_id = ''")
                        .unwrap()
                }
            };
            let mut rows = statement.raw_query();
            loop {
                match receiver.recv() {
                    Ok(cmd) => {
                        match cmd {
                            CursorCommand::Stop => break,
                            CursorCommand::Next(sender) => {
                                let mut results = vec![];
                                loop {
                                    match rows.next() {
                                        Ok(None) => {
                                            // We are out of items. Send the current item list or empty array.
                                            if results.is_empty() {
                                                debug!("ContactDbCursor, empty result");
                                                let _ = sender.send(Some(vec![]));
                                            } else {
                                                debug!("Send results with len:{}", results.len());
                                                let _ = sender.send(Some(results));
                                            }
                                            break;
                                        }
                                        Ok(Some(row)) => {
                                            if let Ok(id) = row_to_contact_id(row) {
                                                debug!("current id is {}", id);
                                                let mut contact = ContactInfo::default();
                                                if let Err(err) =
                                                    contact.fill_main_data(&id, connection)
                                                {
                                                    error!("ContactDbCursor fill_main_data error: {}, continue", err);
                                                    continue;
                                                }

                                                if !only_main_data {
                                                    if let Err(err) = contact
                                                        .fill_additional_data(&id, connection)
                                                    {
                                                        error!("ContactDbCursor fill_additional_data error: {}, continue", err);
                                                        continue;
                                                    }
                                                }
                                                results.push(contact);

                                                if results.len() == batch_size as usize {
                                                    let _ = sender.send(Some(results));
                                                    break;
                                                }
                                            }
                                        }
                                        Err(err) => {
                                            error!("Failed to fetch row: {}", err);
                                            let _ = sender.send(None);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Err(err) => {
                        error!("receiver.recv error: {}", err);
                        break;
                    }
                }
            }
            debug!("Exiting contacts cursor thread");
        });
        Self { sender }
    }
}

pub struct ContactsDb {
    // The underlying sqlite db.
    db: SqliteDb,
    // Handle to the event broadcaster to fire events when changing contacts.
    event_broadcaster: ContactsFactoryEventBroadcaster,
}

impl ContactsDb {
    pub fn new(event_broadcaster: ContactsFactoryEventBroadcaster) -> Self {
        Self {
            db: create_db(),
            event_broadcaster,
        }
    }

    pub fn add_dispatcher(&mut self, dispatcher: &ContactsFactoryEventDispatcher) -> DispatcherId {
        self.event_broadcaster.add(dispatcher)
    }

    pub fn remove_dispatcher(&mut self, id: DispatcherId) {
        self.event_broadcaster.remove(id)
    }

    pub fn clear_contacts(&mut self) -> Result<(), Error> {
        debug!("ContactsDb::clear_contacts");
        let conn = self.db.mut_connection();
        let tx = conn.transaction()?;
        tx.execute("UPDATE speed_dials SET contact_id = ''", NO_PARAMS)?;
        tx.execute("DELETE FROM contact_additional", NO_PARAMS)?;
        tx.execute("DELETE FROM group_contacts", NO_PARAMS)?;
        tx.execute("DELETE FROM contact_main", NO_PARAMS)?;
        tx.commit()?;
        Ok(())
    }

    pub fn remove(&mut self, contact_ids: &[String]) -> Result<(), Error> {
        debug!("ContactsDb::remove contacts");
        let connection = self.db.mut_connection();
        let count: i32 = {
            let mut sql = String::from("SELECT COUNT(*) FROM contact_main WHERE contact_id in (");
            for _i in 1..contact_ids.len() {
                sql += "?,";
            }
            sql += "?)";

            debug!("verify has none exist id in remove sql is: {}", sql);

            let mut stmt = connection.prepare(&sql)?;

            stmt.query_row(contact_ids, |r| Ok(r.get_unwrap(0)))?
        };

        if count != contact_ids.len() as i32 {
            return Err(Error::InvalidContactId(
                "Try to remove none exist contact".to_string(),
            ));
        }

        let mut contacts = vec![];
        let tx = connection.transaction()?;
        {
            for contact_id in contact_ids {
                tx.execute(
                    "DELETE FROM contact_additional WHERE contact_id = ?",
                    &[&contact_id],
                )?;
                tx.execute(
                    "UPDATE speed_dials SET contact_id = '' WHERE contact_id = ?",
                    &[&contact_id],
                )?;
                tx.execute(
                    "DELETE FROM contact_main WHERE contact_id = ?",
                    &[&contact_id],
                )?;
                tx.execute(
                    "DELETE FROM group_contacts WHERE contact_id = ?",
                    &[&contact_id],
                )?;
                let mut contact = ContactInfo::default();
                contact.id = contact_id.to_string();
                contacts.push(contact);
            }
        }
        tx.commit()?;
        let event = ContactsChangeEvent {
            reason: ChangeReason::Remove,
            contacts: Some(contacts),
        };

        self.event_broadcaster.broadcast_contacts_change(event);
        Ok(())
    }

    pub fn save(&mut self, contacts: &[ContactInfo], is_update: bool) -> Result<(), Error> {
        debug!("ContactsDb::add {} contacts", contacts.len());
        let connection = self.db.mut_connection();
        let mut new_contacts = vec![];
        debug!("ContactsDb::save is_update:{}", is_update);
        let tx = connection.transaction()?;
        {
            for contact_info in contacts {
                let mut contact = contact_info.clone();
                if is_update {
                    if contact.id.is_empty() {
                        debug!("Try to update a contact without contact id, ignore it");
                        continue;
                    }
                    contact.updated = SystemTime::from(std::time::SystemTime::now());
                    tx.execute(
                        "DELETE FROM contact_additional WHERE contact_id = ?",
                        &[&contact.id],
                    )?;
                    tx.execute(
                        "DELETE FROM group_contacts WHERE contact_id = ?",
                        &[&contact.id],
                    )?;
                    tx.execute(
                        "DELETE FROM contact_main WHERE contact_id = ?",
                        &[&contact.id],
                    )?;
                    contact.updated = SystemTime::from(std::time::SystemTime::now());
                } else {
                    contact.id = Uuid::new_v4().to_string();
                    contact.published = SystemTime::from(std::time::SystemTime::now());
                }
                debug!("Save current contact id is {}", contact.id);

                if let Err(err) = contact.save_main_data(&tx) {
                    error!("save_main_data error: {}, continue", err);
                    continue;
                }
                if let Err(err) = contact.save_additional_data(&tx) {
                    error!("save_additional_data error: {}, continue", err);
                    continue;
                }
                new_contacts.push(contact);
            }
        }
        tx.commit()?;

        let event = ContactsChangeEvent {
            reason: if is_update {
                ChangeReason::Update
            } else {
                ChangeReason::Create
            },
            contacts: Some(new_contacts),
        };
        self.event_broadcaster.broadcast_contacts_change(event);
        Ok(())
    }

    pub fn get(&self, id: &str, only_main_data: bool) -> Result<ContactInfo, Error> {
        debug!(
            "ContactsDb::get id {}, only_main_data {}",
            id, only_main_data
        );
        let mut contact = ContactInfo::default();
        let conn = self.db.connection();
        contact.fill_main_data(&id, &conn)?;
        if !only_main_data {
            contact.fill_additional_data(&id, &conn)?;
        }
        Ok(contact)
    }

    pub fn count(&self) -> Result<u32, Error> {
        debug!("ContactsDb::count");
        let mut stmt = self
            .db
            .connection()
            .prepare("SELECT COUNT(contact_id) FROM contact_main")?;

        let count = stmt.query_row(NO_PARAMS, |r| Ok(r.get_unwrap(0)))?;

        Ok(count)
    }

    pub fn get_all(
        &self,
        options: ContactSortOptions,
        batch_size: i64,
        only_main_data: bool,
    ) -> Option<ContactDbCursor> {
        debug!(
            "ContactsDb::get_all options {:#?}, batch_size {}, only_main_data {}",
            options, batch_size, only_main_data
        );

        Some(ContactDbCursor::new(
            batch_size,
            only_main_data,
            move |connection| {
                let field: String = options.sort_by.into();
                let order: String = options.sort_order.into();
                debug!("field = {}", field);
                debug!("order = {}", order);

                let sql = format!(
                    "SELECT contact_id FROM contact_main ORDER BY {} COLLATE NOCASE {}",
                    field, order
                );
                debug!("get_all sql is {}", sql);
                let statement = match connection.prepare(&sql) {
                    Ok(statement) => statement,
                    Err(err) => {
                        error!("Failed to prepare `get_all` statement `{}`: {}", sql, err);
                        return None;
                    }
                };
                Some(statement)
            },
        ))
    }

    pub fn find(
        &self,
        options: ContactFindSortOptions,
        batch_size: i64,
    ) -> Option<ContactDbCursor> {
        debug!("ContactsDb::find {:#?}, batch_size {}", options, batch_size);
        Some(ContactDbCursor::new(
            batch_size,
            options.only_main_data,
            move |connection| {
                let mut sql = String::from("SELECT contact_id FROM contact_main WHERE ");
                let mut params = vec![];
                match options.filter_by {
                    FilterByOption::Name => {
                        sql.push_str("name LIKE :value");
                    }
                    FilterByOption::GivenName => {
                        sql.push_str("given_name LIKE :value");
                    }
                    FilterByOption::FamilyName => {
                        sql.push_str("family_name LIKE :value");
                    }
                    FilterByOption::Email => {
                        sql.push_str("email LIKE :value");
                    }
                    FilterByOption::Tel => {
                        sql.push_str("tel_number LIKE :value");
                    }
                    FilterByOption::Category => {
                        sql = String::from(
                            "SELECT contact_id FROM contact_additional WHERE data_type = 'category' 
                            AND value LIKE :value",
                        );
                    }
                }

                let value = match options.filter_option {
                    FilterOption::StartsWith => {
                        if options.filter_by == FilterByOption::Email
                            || options.filter_by == FilterByOption::Tel
                        {
                            // The tel_number and email will store like:"\u{001E}88888\u{001E}99999\u{001E}.
                            // StartsWith means contain %\u{001E}{}%.
                            format!("%\u{001E}{}%", options.filter_value)
                        } else {
                            format!("{}%", options.filter_value)
                        }
                    }
                    FilterOption::FuzzyMatch => {
                        // Only used for tel
                        // Matching from back to front, If the filter_value length is greater
                        // Than MIN_MATCH_DIGITS, take the last MIN_MATCH_DIGITS length string.
                        let filter_value = &options.filter_value;
                        let mut slice = "".to_string();
                        if filter_value.len() > MIN_MATCH_DIGITS {
                            let start = filter_value.len() - MIN_MATCH_DIGITS;
                            if let Some(value_slice) = filter_value.get(start..filter_value.len()) {
                                slice = value_slice.to_string()
                            }
                            format!("%{}\u{001E}%", slice)
                        } else {
                            format!("%{}\u{001E}%", filter_value)
                        }
                    }
                    FilterOption::Contains => format!("%{}%", options.filter_value),
                    FilterOption::Equals => {
                        if options.filter_by == FilterByOption::Email
                            || options.filter_by == FilterByOption::Tel
                        {
                            // The email and tel number will store like:"\u{001E}88888\u{001E}99999\u{001E}".
                            // For equal it means to contains "%\u{001E}{}\u{001E}%".
                            format!("%\u{001E}{}\u{001E}%", options.filter_value)
                        } else {
                            options.filter_value.to_string()
                        }
                    }
                    FilterOption::Match => "".to_string(),
                };

                debug!("find filter value is {}", value);
                params.push(value);

                let order_filed: String = options.sort_by.into();

                if !order_filed.is_empty() && options.filter_by != FilterByOption::Category {
                    sql.push_str(" ORDER BY ");
                    sql.push_str(&order_filed);
                    sql.push_str(&" COLLATE NOCASE ".to_string());
                    let order: String = options.sort_order.into();
                    sql.push_str(&order);
                }

                debug!("find sql is {}", sql);

                let mut statement = match connection.prepare(&sql) {
                    Ok(statement) => statement,
                    Err(err) => {
                        error!("Failed to prepare `find` statement: {} error: {}", sql, err);
                        return None;
                    }
                };

                for n in 0..params.len() {
                    debug!("current n is {}, param = {}", n, params[n]);
                    // SQLite binding indexes are 1 based, not 0 based...
                    if let Err(err) = statement.raw_bind_parameter(n + 1, params[n].to_string()) {
                        error!(
                            "Failed to bind #{} `find` parameter to `{}`: {}",
                            n, sql, err
                        );
                        return None;
                    }
                }

                Some(statement)
            },
        ))
    }

    pub fn set_ice(&mut self, contact_id: &str, position: i64) -> Result<(), Error> {
        let conn = self.db.connection();
        let contact_id_count: i32 = {
            let sql = String::from("SELECT COUNT(*) FROM contact_main WHERE contact_id = ?");
            let mut stmt = conn.prepare(&sql)?;
            stmt.query_row(&[&contact_id], |r| Ok(r.get_unwrap(0)))?
        };

        if contact_id_count != 1 {
            return Err(Error::InvalidContactId(
                "Try to set_ice with invalid contact id".to_string(),
            ));
        }

        let position_count: i32 = {
            let sql = String::from(
                "SELECT COUNT(*) FROM contact_additional WHERE data_type = 'ice_position' AND value = ?",
            );
            let mut stmt = conn.prepare(&sql)?;
            stmt.query_row(&[&position], |r| Ok(r.get_unwrap(0)))?
        };

        if position_count != 0 {
            return Err(Error::IcePositionUsed(
                "Try to set_ice with position already used".to_string(),
            ));
        }

        let item_count: i32 = {
            let sql = String::from(
                "SELECT COUNT(*) FROM contact_additional WHERE data_type = 'ice_position' AND contact_id = ?",
            );
            let mut stmt = conn.prepare(&sql)?;
            stmt.query_row(&[&contact_id], |r| Ok(r.get_unwrap(0)))?
        };

        if item_count != 0 {
            conn.execute_named(
                "UPDATE contact_additional SET value = :position WHERE contact_id = :contact_id
                AND data_type = 'ice_position'",
                &[(":position", &position), (":contact_id", &contact_id)],
            )?;
        } else {
            conn.execute_named(
                "INSERT INTO contact_additional (contact_id, data_type, value) 
                VALUES (:contact_id, 'ice_position', :position)",
                &[(":contact_id", &contact_id), (":position", &position)],
            )?;
        }

        Ok(())
    }

    pub fn remove_ice(&mut self, contact_id: &str) -> Result<(), Error> {
        let conn = self.db.connection();
        let count: i32 = {
            let sql = String::from("SELECT COUNT(*) FROM contact_main WHERE contact_id = ?");
            let mut stmt = conn.prepare(&sql)?;
            stmt.query_row(&[&contact_id], |r| Ok(r.get_unwrap(0)))?
        };

        if count != 1 {
            return Err(Error::InvalidContactId(
                "Try to remove_ice with invalid contact id".to_string(),
            ));
        }

        conn.execute(
            "DELETE FROM contact_additional WHERE contact_id = ? AND data_type = 'ice_position'",
            &[contact_id],
        )?;

        Ok(())
    }

    pub fn get_all_ice(&mut self) -> Result<Vec<IceInfo>, Error> {
        debug!("ContactsDb::get_all_ice");
        let conn = self.db.connection();
        let mut stmt = conn.prepare(
            "SELECT value, contact_id FROM contact_additional WHERE
            data_type = 'ice_position' AND value != '0' ORDER BY value ASC",
        )?;

        let rows = stmt.query_map(NO_PARAMS, |row| {
            Ok(IceInfo {
                position: {
                    let value: String = row.get(0)?;
                    value.parse().unwrap_or(0)
                },
                contact_id: row.get(1)?,
            })
        })?;

        rows_to_vec(rows)
    }

    pub fn import_vcf(&mut self, vcf: &str) -> Result<usize, Error> {
        debug!("import_vcf {}", vcf.len());
        let parser = ical::VcardParser::new(BufReader::new(vcf.as_bytes()));
        let mut contacts = vec![];
        for item in parser {
            if let Ok(vcard) = item {
                // Initialize the contact with default values.
                let mut contact = ContactInfo::default();
                for prop in vcard.properties {
                    if prop.name == "EMAIL" {
                        if let Some(email_vcard) = &prop.value {
                            fillVecField!(
                                contact.email,
                                ContactField {
                                    atype: "".to_string(),
                                    value: email_vcard.clone(),
                                    pref: false,
                                }
                            );
                        }
                    } else if prop.name == "TEL" {
                        if let Some(tel_vcard) = &prop.value {
                            fillVecField!(
                                contact.tel,
                                ContactTelField {
                                    atype: "".to_string(),
                                    value: tel_vcard.clone(),
                                    pref: false,
                                    carrier: "".to_string(),
                                }
                            );
                        }
                    } else if prop.name == "FN" {
                        contact.name = prop.value.unwrap_or_else(|| "".into());
                    } else if prop.name == "TITLE" {
                        fillVecField!(contact.job_title, prop.value.unwrap_or_else(|| "".into()));
                    }
                }
                debug!("contact in vcard is : {:?}", contact);
                contacts.push(contact);
            }
        }
        self.save(&contacts, false).map(|_| contacts.len())
    }

    pub fn add_blocked_number(&mut self, number: &str) -> Result<(), Error> {
        debug!("ContactsDb::add_blocked_number number:{}", number);

        let conn = self.db.connection();
        let mut stmt = conn.prepare("INSERT INTO blocked_numbers (number) VALUES (?)")?;
        let size = stmt.execute(&[number])?;
        if size > 0 {
            let event = BlockedNumberChangeEvent {
                reason: ChangeReason::Create,
                number: number.to_string(),
            };
            self.event_broadcaster.broadcast_blockednumber_change(event);
        }
        debug!("ContactsDb::add_blocked_number OK {}", size);
        Ok(())
    }

    pub fn remove_blocked_number(&mut self, number: &str) -> Result<(), Error> {
        debug!("ContactsDb::remove_blocked_number number:{}", number);

        let conn = self.db.connection();
        let mut stmt = conn.prepare("DELETE FROM blocked_numbers WHERE number = ?")?;
        let size = stmt.execute(&[number])?;
        if size > 0 {
            let event = BlockedNumberChangeEvent {
                reason: ChangeReason::Remove,
                number: number.to_string(),
            };
            self.event_broadcaster.broadcast_blockednumber_change(event);
        }
        debug!("ContactsDb::remove_blocked_number OK size:{}", size);
        Ok(())
    }

    pub fn get_all_blocked_numbers(&mut self) -> Result<Vec<String>, Error> {
        debug!("ContactsDb::get_all_blocked_numbers");
        let conn = self.db.connection();
        let mut stmt = conn.prepare("SELECT number FROM blocked_numbers")?;

        let rows = stmt.query_map(NO_PARAMS, |row| row.get(0))?;
        rows_to_vec(rows)
    }

    pub fn find_blocked_numbers(
        &mut self,
        options: BlockedNumberFindOptions,
    ) -> Result<Vec<String>, Error> {
        debug!("ContactsDb::find_blocked_numbers options:{:?}", &options);
        let conn = self.db.connection();
        let mut stmt =
            conn.prepare("SELECT number FROM blocked_numbers WHERE number LIKE :param")?;

        let param = match options.filter_option {
            FilterOption::StartsWith => format!("{}%", options.filter_value),
            FilterOption::FuzzyMatch => {
                // Matching from back to front, If the filter_value length is greater
                // than MIN_MATCH_DIGITS, take the last MIN_MATCH_DIGITS length string.
                let mut filter_value = options.filter_value;
                if filter_value.len() > MIN_MATCH_DIGITS {
                    let start = filter_value.len() - MIN_MATCH_DIGITS;
                    if let Some(value_slice) = filter_value.get(start..filter_value.len()) {
                        filter_value = value_slice.to_string()
                    }
                }
                format!("%{}", filter_value)
            }
            FilterOption::Contains => format!("%{}%", options.filter_value),
            FilterOption::Equals => options.filter_value,
            FilterOption::Match => {
                return Err(Error::InvalidFilterOption("Match".into()));
            }
        };

        let rows = stmt.query_map_named(&[(":param", &param)], |row| row.get(0))?;
        rows_to_vec(rows)
    }

    pub fn get_speed_dials(&mut self) -> Result<Vec<SpeedDialInfo>, Error> {
        debug!("ContactsDb::get_speed_dials");
        let conn = self.db.connection();
        let mut stmt = conn.prepare("SELECT * FROM speed_dials")?;

        let rows = stmt.query_map(NO_PARAMS, |row| {
            Ok(SpeedDialInfo {
                dial_key: row.get(0)?,
                tel: row.get(1)?,
                contact_id: row.get(2)?,
            })
        })?;

        rows_to_vec(rows)
    }

    pub fn add_speed_dial(
        &mut self,
        dial_key: &str,
        tel: &str,
        contact_id: &str,
    ) -> Result<(), Error> {
        debug!(
            "ContactsDb::add_speed_dial, dial_key:{}, tel:{}, contact_id:{}",
            dial_key, tel, contact_id
        );
        let conn = self.db.connection();
        let mut stmt = conn
            .prepare("INSERT INTO speed_dials (dial_key, tel, contact_id) VALUES (?1, ?2, ?3)")?;
        let size = stmt.execute(&[dial_key, tel, contact_id])?;
        if size > 0 {
            let event = SpeedDialChangeEvent {
                reason: ChangeReason::Create,
                speeddial: SpeedDialInfo {
                    dial_key: dial_key.to_string(),
                    tel: tel.to_string(),
                    contact_id: contact_id.to_string(),
                },
            };
            debug!("ContactsDb::add_speed_dial event ={:?}", event);
            self.event_broadcaster.broadcast_speeddial_change(event);
        }
        debug!("ContactsDb::add_speed_dial Ok {}", size);
        Ok(())
    }

    pub fn update_speed_dial(
        &mut self,
        dial_key: &str,
        tel: &str,
        contact_id: &str,
    ) -> Result<(), Error> {
        debug!(
            "ContactsDb::update_speed_dial, dial_key:{}, tel:{}, contact_id:{}",
            dial_key, tel, contact_id
        );
        let conn = self.db.connection();
        let mut stmt =
            conn.prepare("UPDATE speed_dials SET tel = ?1, contact_id = ?2 WHERE dial_key = ?3")?;
        let size = stmt.execute(&[tel, contact_id, dial_key])?;
        if size > 0 {
            let event = SpeedDialChangeEvent {
                reason: ChangeReason::Update,
                speeddial: SpeedDialInfo {
                    dial_key: dial_key.to_string(),
                    tel: tel.to_string(),
                    contact_id: contact_id.to_string(),
                },
            };
            self.event_broadcaster.broadcast_speeddial_change(event);
        }
        debug!("ContactsDb::update_speed_dial Ok {}", size);
        Ok(())
    }

    pub fn remove_speed_dial(&mut self, dial_key: &str) -> Result<(), Error> {
        debug!("ContactsDb::remove_speed_dial");
        let conn = self.db.connection();
        let mut stmt = conn.prepare("DELETE FROM speed_dials WHERE dial_key = ?")?;
        let size = stmt.execute(&[dial_key])?;
        if size > 0 {
            let event = SpeedDialChangeEvent {
                reason: ChangeReason::Remove,
                speeddial: SpeedDialInfo {
                    dial_key: dial_key.to_string(),
                    tel: "".to_string(),
                    contact_id: "".to_string(),
                },
            };
            self.event_broadcaster.broadcast_speeddial_change(event);
        }
        debug!("ContactsDb::remove_speed_dial Ok {}", size);
        Ok(())
    }

    pub fn remove_group(&mut self, id: &str) -> Result<(), Error> {
        debug!("ContactsDb::remove_group id:{}", id);
        let connection = self.db.mut_connection();
        let tx = connection.transaction()?;
        tx.execute("DELETE FROM group_contacts WHERE group_id is ?", &[id])?;
        tx.execute(
            "DELETE FROM contact_additional WHERE data_type = 'groups' AND value IS ?",
            &[id],
        )?;
        tx.execute("DELETE FROM groups WHERE id is ?", &[id])?;
        tx.commit()?;
        let event = GroupChangeEvent {
            reason: ChangeReason::Remove,
            group: GroupInfo {
                name: "".to_string(),
                id: id.to_string(),
            },
        };
        self.event_broadcaster.broadcast_group_change(event);
        debug!("ContactsDb::remove_group OK ");
        Ok(())
    }

    pub fn add_group(&mut self, name: &str) -> Result<(), Error> {
        debug!("ContactsDb::add_group  name = {}", name);
        let id = Uuid::new_v4().to_string();
        let conn = self.db.connection();
        let size = conn.execute("INSERT INTO groups (id, name) VALUES(?, ?)", &[&id, name])?;
        if size > 0 {
            let event = GroupChangeEvent {
                reason: ChangeReason::Create,
                group: GroupInfo {
                    name: name.to_string(),
                    id,
                },
            };
            self.event_broadcaster.broadcast_group_change(event);
        }
        debug!("ContactsDb::add_group OK size: {}", size);
        Ok(())
    }

    pub fn update_group(&mut self, id: &str, name: &str) -> Result<(), Error> {
        debug!("ContactsDb::update_group id ={}, name= {}", id, name);
        let conn = self.db.connection();
        let size = conn.execute("UPDATE groups SET name = ? WHERE id = ?", &[name, id])?;
        if size > 0 {
            let event = GroupChangeEvent {
                reason: ChangeReason::Update,
                group: GroupInfo {
                    name: name.to_string(),
                    id: id.to_string(),
                },
            };
            self.event_broadcaster.broadcast_group_change(event);
        }
        debug!("ContactsDb::update_group OK size: {}", size);
        Ok(())
    }

    pub fn get_contactids_from_group(&mut self, group_id: &str) -> Result<Vec<String>, Error> {
        debug!(
            "ContactsDb::get_contactids_from_group group_id is :{}",
            group_id
        );
        let conn = self.db.connection();
        let mut stmt =
            conn.prepare("SELECT contact_id FROM group_contacts WHERE group_id IS :group_id")?;
        let rows = stmt.query_map(&[group_id], |row| Ok(row.get(0)?))?;

        rows_to_vec(rows)
    }

    pub fn get_all_groups(&mut self) -> Result<Vec<GroupInfo>, Error> {
        debug!("ContactsDb::get_all_groups");
        let conn = self.db.connection();
        let mut stmt = conn.prepare("SELECT * FROM groups ORDER BY name COLLATE NOCASE ASC")?;

        let rows = stmt.query_map(NO_PARAMS, |row| {
            Ok(GroupInfo {
                name: row.get(1)?,
                id: row.get(0)?,
            })
        })?;

        rows_to_vec(rows)
    }

    pub fn import_sim_contacts(&mut self, sim_contacts: &[SimContactInfo]) -> Result<(), Error> {
        debug!("ContactsDb::import_sim_contacts");

        let contacts_info: Vec<ContactInfo> = sim_contacts.iter().map(|item| item.into()).collect();
        self.save(&contacts_info, true)
    }
}

fn rows_to_vec<I, R>(source: I) -> Result<Vec<R>, Error>
where
    I: core::iter::Iterator<Item = Result<R, rusqlite::Error>>,
{
    Ok(source
        .filter_map(|item| match item {
            Ok(val) => Some(val),
            _ => None,
        })
        .collect())
}

#[cfg(test)]
mod test {
    use super::*;
    #[test]
    fn create_contacts_database() {
        let _ = env_logger::try_init();

        let broadcaster = ContactsFactoryEventBroadcaster::default();

        let mut db = ContactsDb::new(broadcaster);
        db.clear_contacts().unwrap();

        assert_eq!(db.count().unwrap(), 0);

        let mut bob = ContactInfo::default();
        bob.name = "Bob".to_string();

        let mut alice = ContactInfo::default();
        alice.name = "alice".to_string();

        db.save(&[bob, alice], false).unwrap();

        assert_eq!(db.count().unwrap(), 2);

        db.clear_contacts().unwrap();

        assert_eq!(db.count().unwrap(), 0);

        // Import sim contacts.
        let sim_contact_1 = SimContactInfo {
            id: "0001".to_string(),
            tel: "13682628272\u{001E}18812345678\u{001E}19922223333".to_string(),
            email: "test@163.com\u{001E}happy@sina.com\u{001E}3179912@qq.com".to_string(),
            name: "Ted".to_string(),
        };

        let sim_contact_2 = SimContactInfo {
            id: "0002".to_string(),
            tel: "15912345678\u{001E}18923456789".to_string(),
            email: "test1@kaiostech.com\u{001E}231678456@qq.com".to_string(),
            name: "Bob".to_string(),
        };

        db.import_sim_contacts(&[sim_contact_1, sim_contact_2])
            .unwrap();

        assert_eq!(db.count().unwrap(), 2);

        db.clear_contacts().unwrap();

        assert_eq!(db.count().unwrap(), 0);

        // Load contacts from a vcf file.
        let input = std::fs::read_to_string("./test-fixtures/contacts_200.vcf").unwrap();
        debug!("Importing contacts from vcf.");
        let _count = db.import_vcf(&input).unwrap();
        assert_eq!(db.count().unwrap(), 200);
    }
}
