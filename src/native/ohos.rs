use napi_ohos::{bindgen_prelude::Object, Env, Result};
use ohos_xcomponent_binding::{XComponent,WindowRaw};
use std::sync::Once;
use ohos_hilog_binding::{hilog_fatal, hilog_info};
use crate::{
    event::{EventHandler, KeyCode, KeyMods, TouchPhase},
    native::{
        egl::{self, LibEgl},
        NativeDisplayData,
    },
};

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
        hilog_info!("Updating surface");
        self.window = window;
        if self.surface.is_null() == false {
            self.destroy_surface();
        }

        hilog_info!(format!("Creating window surface with window: {:?}", window));
        self.surface = (self.libegl.eglCreateWindowSurface)(
            self.egl_display,
            self.egl_config,
            window.0 as _,
            std::ptr::null_mut(),
        );

        if self.surface.is_null() {
            hilog_fatal!("Failed to create EGL window surface");
            return;
        }

        hilog_info!("Making EGL context current");
        let res = (self.libegl.eglMakeCurrent)(
            self.egl_display,
            self.surface,
            self.surface,
            self.egl_context,
        );

        if res == 0 {
            hilog_fatal!("Failed to make EGL context current");
        } else {
            hilog_info!("EGL context made current successfully");
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
        hilog_info!("Frame rendering started");
        self.event_handler.update();

        if self.surface.is_null() == false {
            self.update_requested = false;
            self.event_handler.draw();

            unsafe {
                hilog_info!("Swapping buffers");
                let result = (self.libegl.eglSwapBuffers)(self.egl_display, self.surface);
                if result == 0 {
                    hilog_fatal!("Failed to swap buffers");
                } else {
                    hilog_info!("Buffers swapped successfully");
                }
            }
        } else {
            hilog_info!("Skipping frame render - surface is null");
        }
    }

    fn process_request(&mut self, request: crate::native::Request) {
        use crate::native::Request::*;

        match request {
            ScheduleUpdate => {
                self.update_requested = true;
            }
            SetFullscreen(fullscreen) => {
                // TODO: Implement fullscreen functionality
            }
            ShowKeyboard(show) => {
                // TODO: Implement keyboard functionality
            }
            _ => {}
        }
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

    struct SendHack<F>(F);
    unsafe impl<F> Send for SendHack<F> {}
    let f = SendHack(f);

    let (tx, rx) = mpsc::channel();

    let tx2 = tx.clone();
    MESSAGES_TX.with(move |messages_tx| *messages_tx.borrow_mut() = Some(tx2));
    thread::spawn(move || {
        hilog_info!("Starting event loop");
        let mut libegl = LibEgl::try_load().expect("Cant load LibEGL");

        // skip all the messages until android will be able to actually open a window
        //
        // sometimes before launching an app android will show a permission dialog
        // it is important to create GL context only after a first SurfaceChanged
        let window = 'a: loop {
            match rx.try_recv() {
                Ok(Message::SurfaceCreated { window }) => {
                    hilog_info!("Message::SurfaceCreated");
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
            conf.platform.framebuffer_alpha,
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
            window,
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

            if !conf.platform.blocking_event_loop || s.update_requested {
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
        hilog_info!("xcomponent_create");
        hilog_info!(format!("Window created: {:?}", win));
        send_message(Message::SurfaceCreated { window: win });
        Ok(())
    });

    xcomponent.on_surface_changed(|_xcomponent, win| {
        hilog_info!("xcomponent_changed");
        // Get the window dimensions
        // TODO: We need to extract width and height from win parameter
        // For now, let's use placeholder values to test
        let width = 800;  // Placeholder
        let height = 600; // Placeholder
        hilog_info!(format!("Surface changed: {}x{}", width, height));
        send_message(Message::SurfaceChanged { width, height });
        Ok(())
    });

    xcomponent.on_surface_destroyed(|_xcomponent, _win| {
        hilog_info!("xcomponent_destroy");
        send_message(Message::SurfaceDestroyed);
        Ok(())
    });

    xcomponent.on_touch_event(|_xcomponent, _win, data| {
        hilog_info!("xcomponent_dispatch");
        hilog_info!(format!("xcomponent_dispatch: {:?}", data));
        
        // Send touch events to main thread
        // TODO: Properly handle touch data when we know the structure
        /*
        if let Some(touch) = data.first() {
            send_message(Message::Touch {
                phase: TouchPhase::Moved, // TODO: Map properly based on touch data
                touch_id: touch.id as u64,
                x: touch.x,
                y: touch.y,
            });
        }
        */
        Ok(())
    });

    xcomponent.register_callback();

    xcomponent.on_frame_callback(|_, _, _| {
        //hilog_info!("xcomponent_frame");
        // Process pending messages in frame callback
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