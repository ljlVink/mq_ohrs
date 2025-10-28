use ohos_xcomponent_binding::{XComponent,WindowRaw};
use ohos_xcomponent_sys::{
    OH_NativeXComponent,OH_NativeXComponent_RegisterKeyEventCallback, OH_NativeXComponent_GetKeyEvent, OH_NativeXComponent_GetKeyEventAction,
    OH_NativeXComponent_GetKeyEventCode
};
use ohos_hilog_binding::{hilog_error, hilog_fatal, hilog_info};
use crate::{
    event::{EventHandler, KeyCode, KeyMods, TouchPhase},
    native::{
        egl::{self, LibEgl},
        NativeDisplayData,
    },
};
mod keycodes;

use crate::{OHOS_ENV, OHOS_EXPORTS};
use std::{cell::RefCell, sync::mpsc, thread};

#[derive(Debug)]
enum Message {
    SurfaceChanged {
        width: i32,
        height: i32,
    },
    SurfaceCreated {
        window: WindowRaw,
    },
    SurfaceDestroyed,
    Touch {
        phase: TouchPhase,
        touch_id: u64,
        x: f32,
        y: f32,
    },
    Character {
        character: u32,
    },
    KeyDown {
        keycode: KeyCode,
    },
    KeyUp {
        keycode: KeyCode,
    },
    Pause,
    Resume,
    Destroy,
    Request(crate::native::Request),
}

unsafe impl Send for Message {}

thread_local! {
    static MESSAGES_TX: RefCell<Option<mpsc::Sender<Message>>> = RefCell::new(None);
}

fn send_message(message: Message) {
    MESSAGES_TX.with(|tx| {
        let mut tx = tx.borrow_mut();
        tx.as_mut().unwrap().send(message).unwrap();
    })
}

struct MainThreadState {
    libegl: LibEgl,
    egl_display: egl::EGLDisplay,
    egl_config: egl::EGLConfig,
    egl_context: egl::EGLContext,
    surface: egl::EGLSurface,
    window:  WindowRaw,
    event_handler: Box<dyn EventHandler>,
    quit: bool,
    fullscreen: bool,
    update_requested: bool,
    keymods: KeyMods,
}

impl MainThreadState {
    unsafe fn destroy_surface(&mut self) {
        (self.libegl.eglMakeCurrent)(
            self.egl_display,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
        (self.libegl.eglDestroySurface)(self.egl_display, self.surface);
        self.surface = std::ptr::null_mut();
    }

    unsafe fn update_surface(&mut self, window: WindowRaw) {
        self.window = window;
        if self.surface.is_null() == false {
            self.destroy_surface();
        }
        self.surface = (self.libegl.eglCreateWindowSurface)(
            self.egl_display,
            self.egl_config,
            window.0 as _,
            std::ptr::null_mut(),
        );

        if self.surface.is_null() {
            let error = (self.libegl.eglGetError)();
            hilog_fatal!(format!("Failed to create EGL window surface, EGL error: {}", error));
            // Additional debugging information
            return;
        }
        let res = (self.libegl.eglMakeCurrent)(
            self.egl_display,
            self.surface,
            self.surface,
            self.egl_context,
        );

        if res == 0 {
            let error = (self.libegl.eglGetError)();
            hilog_fatal!(format!("Failed to make EGL context current, EGL error: {}", error));
        }
    }

    fn process_message(&mut self, msg: Message) {
        hilog_info!("Processing message: {:?}", msg);
        match msg {
            Message::SurfaceCreated { window } => unsafe {
                self.update_surface(window);
            },
            Message::SurfaceDestroyed => unsafe {
                self.destroy_surface();
            },
            Message::SurfaceChanged { width, height } => {
                {
                    let mut d = crate::native_display().lock().unwrap();
                    d.screen_width = width as _;
                    d.screen_height = height as _;
                }
                self.event_handler.resize_event(width as _, height as _);
                if self.surface.is_null() {
                    hilog_info!("Received SurfaceChanged but no surface exists yet");
                }
            }
            Message::Touch {
                phase,
                touch_id,
                x,
                y,
            } => {
                self.event_handler.touch_event(phase, touch_id, x, y);
            }
            Message::Character { character } => {
                if let Some(character) = char::from_u32(character) {
                    self.event_handler
                        .char_event(character, Default::default(), false);
                }
            }
            Message::KeyDown { keycode } => {
                match keycode {
                    KeyCode::LeftShift | KeyCode::RightShift => self.keymods.shift = true,
                    KeyCode::LeftControl | KeyCode::RightControl => self.keymods.ctrl = true,
                    KeyCode::LeftAlt | KeyCode::RightAlt => self.keymods.alt = true,
                    KeyCode::LeftSuper | KeyCode::RightSuper => self.keymods.logo = true,
                    _ => {}
                }
                self.event_handler
                    .key_down_event(keycode, self.keymods, false);
            }
            Message::KeyUp { keycode } => {
                match keycode {
                    KeyCode::LeftShift | KeyCode::RightShift => self.keymods.shift = false,
                    KeyCode::LeftControl | KeyCode::RightControl => self.keymods.ctrl = false,
                    KeyCode::LeftAlt | KeyCode::RightAlt => self.keymods.alt = false,
                    KeyCode::LeftSuper | KeyCode::RightSuper => self.keymods.logo = false,
                    _ => {}
                }
                self.event_handler.key_up_event(keycode, self.keymods);
            }
            Message::Pause => self.event_handler.window_minimized_event(),
            Message::Resume => {
                self.event_handler.window_restored_event()
            }
            Message::Destroy => {
                self.quit = true;
                self.event_handler.quit_requested_event()
            }
            Message::Request(req) => self.process_request(req),

        }
    }

    fn frame(&mut self) {
        self.event_handler.update();
        if !self.surface.is_null() {
            self.update_requested = false;
            self.event_handler.draw();
            unsafe {
                (self.libegl.eglSwapBuffers)(self.egl_display, self.surface);
                
            }
        }
    }

    fn process_request(&mut self, request: crate::native::Request) {
        use crate::native::Request::*;

        match request {
            ScheduleUpdate => {
                self.update_requested = true;
            }
            SetFullscreen(fullscreen) => {
                
            }
            ShowKeyboard(show) => {
                
            }
            _ => {}
        }
    }
}
fn register_xcomponent_callbacks(xcomponent: &XComponent) -> napi_ohos::Result<()> {
    let nativeXComponent = xcomponent.raw();
    let res = unsafe {
        OH_NativeXComponent_RegisterKeyEventCallback(nativeXComponent, Some(on_dispatch_key_event))
    };
    if res != 0 {
        hilog_error!("Failed to register key event callbacks");
    } else {
        hilog_error!("Registered key event callbacks successfully");
    }

    Ok(())
}

pub unsafe extern "C" fn on_dispatch_key_event(
    xcomponent: *mut OH_NativeXComponent,
    window: *mut std::os::raw::c_void,
)
{
    hilog_info!("DispatchKeyEvent");
    let mut event = std::ptr::null_mut();
    let ret = OH_NativeXComponent_GetKeyEvent(xcomponent, &mut event);
    assert!(ret == 0, "Get key event failed");

    let mut action = 0;
    let ret = OH_NativeXComponent_GetKeyEventAction(event, &mut action);
    assert!(ret == 0, "Get key event action failed");

    let mut code = 0;
    let ret = OH_NativeXComponent_GetKeyEventCode(event, &mut code);
    assert!(ret == 0, "Get key event code failed");

    let keycode = keycodes::translate_keycode(code);
    hilog_info!("DispatchKeyEvent: {:?}, action: {:?}, keycode = {:?}", keycode, action, code);
    if(action == 0){
        //down
        send_message(Message::KeyDown { keycode });
    }
    else if (action == 1){
        //up
        send_message(Message::KeyUp { keycode });
    }
}

pub unsafe fn run<F>(conf: crate::conf::Conf, f: F)
where
    F: 'static + FnOnce() -> Box<dyn EventHandler>,
{
    let env = OHOS_ENV.as_ref().expect("OHOS_ENV is not initialized");
    let exports = OHOS_EXPORTS.as_ref().expect("OHOS_EXPORTS is not initialized");
    let xcomponent = XComponent::init(*env, *exports).expect("Failed to initialize XComponent");
    use std::panic;
    panic::set_hook(Box::new(|info|{
        hilog_fatal!(info)
    }));
    register_xcomponent_callbacks(&xcomponent);
    struct SendHack<F>(F);
    unsafe impl<F> Send for SendHack<F> {}
    let f = SendHack(f);

    let (tx, rx) = mpsc::channel();

    let tx2 = tx.clone();
    MESSAGES_TX.with(move |messages_tx| *messages_tx.borrow_mut() = Some(tx2));
    thread::spawn(move || {
        let mut libegl = LibEgl::try_load().expect("Cant load LibEGL");
        // skip all the messages until android will be able to actually open a window
        //
        // sometimes before launching an app android will show a permission dialog
        // it is important to create GL context only after a first SurfaceChanged
        let window = 'a: loop {
            match rx.try_recv() {
                Ok(Message::SurfaceCreated { window }) => {
                    break 'a window;
                }
                _ => {}
            }
        };
        let (screen_width, screen_height) = 'a: loop {
            match rx.try_recv() {
                Ok(Message::SurfaceChanged { width, height }) => {
                    break 'a (width as f32, height as f32);
                }
                _ => {}
            }
        };
        let (egl_context, egl_config, egl_display) = crate::native::egl::create_egl_context(
            &mut libegl,
            std::ptr::null_mut(), /* EGL_DEFAULT_DISPLAY */
            true, // force set rgba 8888 for ohos
            conf.sample_count,
        )
        .expect("Cant create EGL context");

        assert!(!egl_display.is_null());
        assert!(!egl_config.is_null());

        crate::native::gl::load_gl_funcs(|proc| {
            let name = std::ffi::CString::new(proc).unwrap();
            (libegl.eglGetProcAddress)(name.as_ptr() as _)
        });

        let surface = (libegl.eglCreateWindowSurface)(
            egl_display,
            egl_config,
            window.0 as _,
            std::ptr::null_mut(),
        );

        if (libegl.eglMakeCurrent)(egl_display, surface, surface, egl_context) == 0 {
            panic!();
        }

        let clipboard = Box::new(OHOSClipboard {});
        let tx_fn = Box::new(move |req| tx.send(Message::Request(req)).unwrap());
        crate::set_or_replace_display(NativeDisplayData {
            high_dpi: conf.high_dpi,
            blocking_event_loop: conf.platform.blocking_event_loop,
            ..NativeDisplayData::new(screen_width as _, screen_height as _, tx_fn, clipboard)
        });

        let event_handler = f.0();
        let mut s = MainThreadState {
            libegl,
            egl_display,
            egl_config,
            egl_context,
            surface,
            window: WindowRaw(std::ptr::null_mut()), // Will be set when we create the surface
            event_handler,
            quit: false,
            fullscreen: conf.fullscreen,
            update_requested: true,
            keymods: KeyMods {
                shift: false,
                ctrl: false,
                alt: false,
                logo: false,
            },
        };
        unsafe {
            s.update_surface(window);
            if s.surface.is_null() {
                hilog_fatal!("Failed to create initial EGL surface");
                return;
            }
        }

        while !s.quit {
            let block_on_wait = conf.platform.blocking_event_loop && !s.update_requested;

            if block_on_wait {
                let res = rx.recv();

                if let Ok(msg) = res {
                    s.process_message(msg);
                }
            } else {
                // process all the messages from the main thread
                while let Ok(msg) = rx.try_recv() {
                    s.process_message(msg);
                }
            }

            // Only render if we have a valid surface or if update is requested
            if !s.surface.is_null() && (!conf.platform.blocking_event_loop || s.update_requested) {
                s.frame();
            }

            thread::yield_now();
        }

        (s.libegl.eglMakeCurrent)(
            s.egl_display,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
        (s.libegl.eglDestroySurface)(s.egl_display, s.surface);
        (s.libegl.eglDestroyContext)(s.egl_display, s.egl_context);
        (s.libegl.eglTerminate)(s.egl_display);
    });
    
    xcomponent.on_surface_created(|xcomponent, win: WindowRaw| {
        send_message(Message::SurfaceCreated { window: win });
        let sz = xcomponent.size(win)?;
        let width = sz.width as i32;
        let height = sz.height as i32;
        send_message(Message::SurfaceChanged { width, height });
        Ok(())
    });

    xcomponent.on_surface_changed(|xcomponent, win| {
        let sz = xcomponent.size(win)?;
        let width = sz.width as i32;
        let height = sz.height as i32;
        send_message(Message::SurfaceChanged { width, height });
        Ok(())
    });

    xcomponent.on_surface_destroyed(|_xcomponent, _win| {
        
        send_message(Message::SurfaceDestroyed);
        Ok(())
    });

    xcomponent.on_touch_event(|_xcomponent, _win, data| {
        if let Some(touch_point) = data.touch_points.first() {
            let phase = match data.event_type {
                ohos_xcomponent_binding::TouchEvent::Down => TouchPhase::Started,
                ohos_xcomponent_binding::TouchEvent::Up => TouchPhase::Ended,
                ohos_xcomponent_binding::TouchEvent::Move => TouchPhase::Moved,
                ohos_xcomponent_binding::TouchEvent::Cancel => TouchPhase::Cancelled,
                _ => TouchPhase::Cancelled, // Default to cancelled for unknown events
            };
            send_message(Message::Touch {
                phase,
                touch_id: touch_point.id as u64,
                x: touch_point.x,
                y: touch_point.y,
            });
        }
        Ok(())
    });
    xcomponent.register_callback();

    xcomponent.on_frame_callback(|_, _, _| {
        Ok(())
    });

}

struct OHOSClipboard;

impl crate::native::Clipboard for OHOSClipboard {
    fn get(&mut self) -> Option<String> {
        // TODO: Implement clipboard get functionality
        None
    }

    fn set(&mut self, _data: &str) {
        // TODO: Implement clipboard set functionality
    }
}


pub fn load_file<F: Fn(crate::fs::Response) + 'static>(path: &str, on_loaded: F) {
    let response = load_file_sync(path);
    on_loaded(response);
}

fn load_file_sync(path: &str) -> crate::fs::Response {
    use std::fs::File;
    use std::io::Read;
    let full_path: String = format!("/data/storage/el1/bundle/entry/resources/resfile/{}", path);    
    let mut response = vec![];
    let mut file = File::open(&full_path)?;
    file.read_to_end(&mut response)?;
    Ok(response)
}