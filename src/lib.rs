// Copyright 2025 Justin Hu
//
// SPDX-License-Identifier: MIT

use std::{
    cell::{RefCell, RefMut},
    collections::VecDeque,
    ops::{Deref, DerefMut},
    rc::Rc,
};

use wasm_bindgen::prelude::*;
use web_sys::{
    BinaryType, CloseEvent, Event, MessageEvent, WebSocket,
    js_sys::{ArrayBuffer, JsString, Uint8Array},
};

pub type Handler<T> = Option<Box<dyn FnMut(T)>>;

pub enum Message {
    Text(String),
    Binary(Box<[u8]>),
}

struct HandlerCell<T> {
    function: RefCell<Handler<T>>,
    replacement: RefCell<Option<Handler<T>>>,
}
struct HandlerRef<'a, T> {
    function: RefMut<'a, Handler<T>>,
    replacement: &'a RefCell<Option<Handler<T>>>,
}
impl<T> HandlerCell<T> {
    fn new() -> Self {
        Self {
            function: RefCell::new(None),
            replacement: RefCell::new(None),
        }
    }
    fn borrow_mut(&'_ self) -> HandlerRef<'_, T> {
        HandlerRef {
            function: self.function.borrow_mut(),
            replacement: &self.replacement,
        }
    }
    fn replace(&self, new_handler: Option<Box<dyn FnMut(T)>>) -> bool {
        match self.function.try_borrow_mut() {
            Ok(mut old_handler) => {
                *old_handler = new_handler;
                true
            }
            Err(_) => {
                *self.replacement.borrow_mut() = Some(new_handler);
                false
            }
        }
    }
}
impl<T> Deref for HandlerRef<'_, T> {
    type Target = Handler<T>;

    fn deref(&self) -> &Self::Target {
        &self.function
    }
}
impl<T> DerefMut for HandlerRef<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.function
    }
}
impl<T> Drop for HandlerRef<'_, T> {
    fn drop(&mut self) {
        if let Some(replacement) = self.replacement.borrow_mut().take() {
            *self.function = replacement;
        }
    }
}

pub struct WebSocketClient {
    raw_ws: WebSocket,
    _raw_on_open: Option<EventListener>,
    _raw_on_message: EventListener,
    _raw_on_error: EventListener,
    _raw_on_close: EventListener,
    queue: Rc<RefCell<VecDeque<Message>>>,
    error: Rc<RefCell<Option<JsValue>>>,
    on_message: Rc<HandlerCell<Message>>,
    on_error: Rc<HandlerCell<JsValue>>,
}
impl WebSocketClient {
    pub fn new(url: &str, init_message: Option<Message>) -> Result<Self, JsValue> {
        let queue = Rc::new(RefCell::new(VecDeque::new()));
        let error = Rc::new(RefCell::new(None));

        let on_message = Rc::new(HandlerCell::new());
        let on_error = Rc::new(HandlerCell::new());

        let raw_ws = WebSocket::new(url)?;
        raw_ws.set_binary_type(BinaryType::Arraybuffer);

        Ok(Self {
            raw_ws: raw_ws.clone(),
            _raw_on_open: init_message.map(|message| {
                EventListener::new(raw_ws.clone().into(), "open", {
                    let on_open_raw_ws = raw_ws.clone();
                    let on_open_error = error.clone();
                    let handler = on_error.clone();
                    move |_| {
                        let mut handler = handler.borrow_mut();
                        let send_attempt = match &message {
                            Message::Text(message) => on_open_raw_ws.send_with_str(message),
                            Message::Binary(message) => on_open_raw_ws.send_with_u8_array(message),
                        };
                        if let Err(err) = send_attempt {
                            if let Some(ref mut handler) = *handler {
                                handler(err);
                            } else {
                                on_open_error.borrow_mut().replace(err);
                            }
                        }
                    }
                })
            }),
            _raw_on_message: EventListener::new(raw_ws.clone().into(), "message", {
                let on_message_queue = queue.clone();
                let handler = on_message.clone();
                move |msg| {
                    let msg = msg
                        .dyn_into::<MessageEvent>()
                        .expect("parameter of websocket message callback");
                    let mut handler = handler.borrow_mut();
                    let msg = if let Ok(msg) = msg.data().dyn_into::<ArrayBuffer>() {
                        let array = Uint8Array::new(&msg);
                        Message::Binary(array.to_vec().into_boxed_slice())
                    } else if let Ok(msg) = msg.data().dyn_into::<JsString>() {
                        Message::Text(msg.into())
                    } else {
                        // bail - not recognized binary or text message
                        return;
                    };
                    if let Some(ref mut handler) = *handler {
                        handler(msg);
                    } else {
                        on_message_queue.borrow_mut().push_back(msg);
                    }
                }
            }),
            _raw_on_error: EventListener::new(raw_ws.clone().into(), "error", {
                let on_error_cell = error.clone();
                let handler = on_error.clone();
                move |error| {
                    let mut handler = handler.borrow_mut();
                    if let Some(ref mut handler) = *handler {
                        handler(error.into());
                    } else {
                        *on_error_cell.borrow_mut() = Some(error.into());
                    }
                }
            }),
            _raw_on_close: EventListener::new(raw_ws.clone().into(), "close", {
                let on_close_cell = error.clone();
                let error_handler = on_error.clone();
                let on_message_queue = queue.clone();
                let message_handler = on_message.clone();
                move |event| {
                    let close_event = event.dyn_into::<CloseEvent>();
                    match close_event {
                        Ok(event) if event.was_clean() => {
                            let mut handler = message_handler.borrow_mut();
                            if let Some(ref mut handler) = *handler {
                                handler(Message::Text(event.reason()));
                            } else {
                                on_message_queue.borrow_mut().push_back(Message::Text(event.reason()));
                            }
                        }
                        Ok(event) => {
                            let mut handler = error_handler.borrow_mut();
                            if let Some(ref mut handler) = *handler {
                                handler(event.into());
                            } else {
                                *on_close_cell.borrow_mut() = Some(event.into())
                            }
                        }
                        Err(event) => {
                            let mut handler = error_handler.borrow_mut();
                            if let Some(ref mut handler) = *handler {
                                handler(event.into());
                            } else {
                                *on_close_cell.borrow_mut() = Some(event.into());
                            }
                        }
                    }
                }
            }),
            queue,
            error,
            on_message,
            on_error,
        })
    }

    pub fn send(&mut self, message: &str) {
        if let Err(err) = self.raw_ws.send_with_str(message) {
            self.report_error(err);
        }
    }

    pub fn set_onmessage(&mut self, new_handler: Option<Box<dyn FnMut(Message)>>) {
        if self.on_message.replace(new_handler) {
            while let Some(ref mut handler) = *self.on_message.borrow_mut()
                && let Some(message) = self.queue.borrow_mut().pop_front()
            {
                handler(message);
            }
        }
    }

    pub fn set_onerror(&mut self, new_handler: Option<Box<dyn FnMut(JsValue)>>) {
        self.on_error.replace(new_handler);
        if let Some(ref mut handler) = *self.on_error.borrow_mut()
            && let Some(error) = self.error.borrow_mut().take()
        {
            handler(error);
        }
    }

    fn report_error(&mut self, err: JsValue) {
        if let Some(ref mut handler) = *self.on_error.borrow_mut() {
            handler(err);
        } else {
            self.error.borrow_mut().replace(err);
        }
    }
}

struct EventListener {
    target: web_sys::EventTarget,
    name: &'static str,
    callback: Closure<dyn FnMut(Event)>,
}
impl EventListener {
    fn new<F>(target: web_sys::EventTarget, name: &'static str, callback: F) -> Self
    where
        F: FnMut(Event) + 'static,
    {
        let callback = Closure::wrap(Box::new(callback) as Box<dyn FnMut(Event)>);
        target
            .add_event_listener_with_callback(name, callback.as_ref().unchecked_ref())
            .unwrap();

        Self {
            target,
            name,
            callback,
        }
    }
}
impl Drop for EventListener {
    fn drop(&mut self) {
        self.target
            .remove_event_listener_with_callback(self.name, self.callback.as_ref().unchecked_ref())
            .unwrap();
    }
}
