/// Implementation of the test service.
use crate::db::SettingsDb;
use crate::generated::common::*;
use crate::generated::service::*;
use common::core::BaseMessage;
use common::traits::{
    DispatcherId, OriginAttributes, Service, SessionSupport, Shared, SharedSessionContext,
    TrackerId,
};
use log::{error, info};
use std::collections::HashMap;
use std::thread;

pub struct SettingsSharedData {
    pub db: SettingsDb,
}

lazy_static! {
    pub(crate) static ref SETTINGS_SHARED_DATA: Shared<SettingsSharedData> =
        Shared::adopt(SettingsSharedData {
            db: SettingsDb::new(SettingsFactoryEventBroadcaster::default())
        });
}

pub struct SettingsService {
    id: TrackerId,
    proxy_tracker: SettingsManagerProxyTracker,
    state: Shared<SettingsSharedData>,
    dispatcher_id: DispatcherId,
    observers: HashMap<ObjectRef, (String, DispatcherId)>,
    origin_attributes: OriginAttributes,
}

impl SettingsManager for SettingsService {
    fn get_proxy_tracker(&mut self) -> &mut SettingsManagerProxyTracker {
        &mut self.proxy_tracker
    }
}

impl SettingsService {
    fn has_permission(&self, perm_name: &str, reason: &str) -> bool {
        let identity = self.origin_attributes.identity();
        if identity == "fake-identity" || identity == "uds" {
            // Grant all permissions when in fake-tokens mode or for uds sessions.
            true
        } else {
            let has_perm = self.origin_attributes.has_permission(perm_name);
            if !has_perm {
                error!(
                    "Failed to {}: {} lacks the {} permission.",
                    reason,
                    self.origin_attributes.identity(),
                    perm_name
                );
            }
            has_perm
        }
    }
}

impl SettingsFactoryMethods for SettingsService {
    fn clear(&mut self, responder: &SettingsFactoryClearResponder) {
        if !self.has_permission("settings:write", "clear settings") {
            responder.permission_error("settings:write", "clear settings");
            return;
        }

        match self.state.lock().db.clear() {
            Ok(()) => responder.resolve(),
            Err(_) => responder.reject(),
        }
    }

    fn get(&mut self, responder: &SettingsFactoryGetResponder, name: String) {
        if !self.has_permission("settings:read", "get setting") {
            responder.permission_error("settings:read", "get settings");
            return;
        }

        let responder = responder.clone();
        let shared = self.state.clone();
        thread::spawn(move || {
            let db = &shared.lock().db;
            match db.get(&name) {
                Ok(value) => responder.resolve(SettingInfo { name, value }),
                Err(crate::db::Error::Sqlite(rusqlite::Error::QueryReturnedNoRows)) => responder
                    .reject(GetError {
                        name,
                        reason: GetErrorReason::NonExistingSetting,
                    }),
                Err(err) => {
                    error!("db get error {:?}", err);
                    responder.reject(GetError {
                        name,
                        reason: GetErrorReason::UnknownError,
                    })
                }
            }
        });
    }

    fn set(&mut self, responder: &SettingsFactorySetResponder, settings: Vec<SettingInfo>) {
        if !self.has_permission("settings:write", "set setting") {
            responder.permission_error("settings:write", "set settings");
            return;
        }

        let responder = responder.clone();
        let shared = self.state.clone();
        thread::spawn(move || {
            let db = &mut shared.lock().db;
            match db.set(&settings) {
                Ok(_) => responder.resolve(),
                Err(_) => responder.reject(),
            }
        });
    }

    fn get_batch(&mut self, responder: &SettingsFactoryGetBatchResponder, names: Vec<String>) {
        if !self.has_permission("settings:read", "get batch of settings") {
            responder.permission_error("settings:read", "get batch of settings");
            return;
        }

        let responder = responder.clone();
        let shared = self.state.clone();
        thread::spawn(move || {
            let db = &shared.lock().db;
            match db.get_batch(&names) {
                Ok(values) => responder.resolve(values),
                Err(_) => responder.reject(),
            }
        });
    }

    fn add_observer(
        &mut self,
        responder: &SettingsFactoryAddObserverResponder,
        name: String,
        observer: ObjectRef,
    ) {
        info!("Adding observer {:?}", observer);
        if !self.has_permission("settings:read", "add setting observer") {
            responder.permission_error("settings:read", "add setting observer");
            return;
        }

        match self.proxy_tracker.get(&observer) {
            Some(SettingsManagerProxy::SettingObserver(proxy)) => {
                let id = self.state.lock().db.add_observer(&name, proxy);
                self.observers.insert(observer, (name, id));
                responder.resolve();
            }
            _ => {
                error!("Failed to get tracked observer");
                responder.reject();
            }
        }
    }

    fn remove_observer(
        &mut self,
        responder: &SettingsFactoryRemoveObserverResponder,
        name: String,
        observer: ObjectRef,
    ) {
        if !self.has_permission("settings:read", "remove setting observer") {
            responder.permission_error("settings:read", "remove setting observer");
            return;
        }

        if self.proxy_tracker.contains_key(&observer) {
            match self.observers.get(&observer) {
                Some((_name, id)) => {
                    self.state.lock().db.remove_observer(&name, *id);
                    responder.resolve();
                }
                None => {
                    error!("Failed to find observer in list");
                    responder.reject();
                }
            }
        } else {
            error!("Failed to find proxy for this observer");
            responder.reject();
        }
    }
}

impl Service<SettingsService> for SettingsService {
    // Shared among instances.
    type State = SettingsSharedData;

    fn shared_state() -> Shared<Self::State> {
        let shared = &*SETTINGS_SHARED_DATA;
        shared.clone()
    }

    fn create(
        origin_attributes: &OriginAttributes,
        _context: SharedSessionContext,
        state: Shared<Self::State>,
        helper: SessionSupport,
    ) -> Option<SettingsService> {
        info!("SettingsService::create");
        let service_id = helper.session_tracker_id().service();
        let event_dispatcher = SettingsFactoryEventDispatcher::from(helper, 0 /* object id */);
        let dispatcher_id = state.lock().db.add_dispatcher(&event_dispatcher);
        Some(SettingsService {
            id: service_id,
            proxy_tracker: HashMap::new(),
            state,
            dispatcher_id,
            observers: HashMap::new(),
            origin_attributes: origin_attributes.clone(),
        })
    }

    // Returns a human readable version of the request.
    fn format_request(&mut self, _transport: &SessionSupport, message: &BaseMessage) -> String {
        let req: Result<SettingsManagerFromClient, common::BincodeError> =
            common::deserialize_bincode(&message.content);
        match req {
            Ok(req) => format!("SettingsService request: {:?}", req),
            Err(err) => format!("Unable to format SettingsService request: {:?}", err),
        }
    }

    // Processes a request coming from the Session.
    fn on_request(&mut self, transport: &SessionSupport, message: &BaseMessage) {
        self.dispatch_request(transport, message);
    }

    fn release_object(&mut self, object_id: u32) -> bool {
        info!("releasing object {}", object_id);
        self.proxy_tracker.remove(&object_id.into()).is_some()
    }
}

impl Drop for SettingsService {
    fn drop(&mut self) {
        info!("Dropping Settings Service #{}", self.id);
        let db = &mut self.state.lock().db;
        db.remove_dispatcher(self.dispatcher_id);

        // Unregister observers for this instance.
        for observer in self.observers.values() {
            db.remove_observer(&observer.0, observer.1);
        }
    }
}
