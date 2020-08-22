use crate::ContextHandle;
use crate::WindowHandle;
use crate::EventHandlerOutput;
use crate::Image;
use crate::WindowId;
use crate::WindowOptions;
use crate::error::EventLoopClosedError;
use crate::error::ProxyCreateWindowError;
use crate::error::ProxyWindowOperationError;
use crate::event::Event;
use crate::event::WindowEvent;
use crate::oneshot;

/// Shorthand type alias for the correct `winit::event::EventLoopProxy`.
type EventLoopProxy<UserEvent> = winit::event_loop::EventLoopProxy<ContextEvent<UserEvent>>;

/// A proxy object to interact with the global context from a different thread.
pub struct ContextProxy<UserEvent: 'static> {
	event_loop: EventLoopProxy<UserEvent>,
}

/// A proxy object to interact with a window from a different thread.
#[derive(Clone)]
pub struct WindowProxy<UserEvent: 'static> {
	window_id: WindowId,
	context_proxy: ContextProxy<UserEvent>,
}

impl<UserEvent: 'static> Clone for ContextProxy<UserEvent> {
	fn clone(&self) -> Self {
		Self { event_loop: self.event_loop.clone() }
	}
}

/// An event that can be sent to the global context.
///
/// It can be either a [`ContextCommand`] or a user event.
pub enum ContextEvent<UserEvent: 'static> {
	ExecuteFunction(ExecuteFunction<UserEvent>),
	UserEvent(UserEvent),
}

pub struct ExecuteFunction<UserEvent: 'static> {
	pub function: Box<dyn FnOnce(&mut ContextHandle<UserEvent>) + Send>,
}

impl<UserEvent> From<ExecuteFunction<UserEvent>> for ContextEvent<UserEvent> {
	fn from(other: ExecuteFunction<UserEvent>) -> Self {
		ContextEvent::ExecuteFunction(other)
	}
}

impl<UserEvent> ContextProxy<UserEvent> {
	/// Wrap an [`EventLoopProxy`] in a [`ContextProxy`].
	pub(crate) fn new(event_loop: EventLoopProxy<UserEvent>) -> Self {
		Self { event_loop }
	}

	/// Create a new window.
	///
	/// The real work is done in the context thread.
	/// This function blocks until the context thread has performed the action.
	pub fn create_window(
		&self,
		title: impl Into<String>,
		options: WindowOptions,
	) -> Result<WindowProxy<UserEvent>, ProxyCreateWindowError> {
		let title = title.into();
		let window_id = self.run_function_wait(move |context| {
			context.create_window(title, options)
				.map(|window| window.id())
		})??;

		Ok(WindowProxy::new(window_id, self.clone()))
	}

	/// Destroy a window.
	///
	/// The real work is done in the context thread.
	/// This function blocks until the context thread has performed the action.
	pub fn destroy_window(
		&self,
		window_id: WindowId,
	) -> Result<(), ProxyWindowOperationError> {
		self.run_function_wait(move |context| {
			context.destroy_window(window_id)
		})??;
		Ok(())
	}

	/// Make a window visiable or invsible.
	///
	/// The real work is done in the context thread.
	/// This function blocks until the context thread has performed the action.
	pub fn set_window_visible(
		&self,
		window_id: WindowId,
		visible: bool,
	) -> Result<(), ProxyWindowOperationError> {
		self.run_function_wait(move |context| {
			context.set_window_visible(window_id, visible)
		})??;
		Ok(())
	}

	/// Set the shown image for a window.
	///
	/// The real work is done in the context thread.
	/// This function blocks until the context thread has performed the action.
	pub fn set_window_image(
		&self,
		window_id: WindowId,
		name: impl Into<String>,
		image: impl Into<Image<'static>>,
	) -> Result<(), ProxyWindowOperationError> {
		let name = name.into();
		let image = image.into();
		self.run_function_wait(move |context| {
			context.set_window_image(window_id, &name, &image)
		})??;
		Ok(())
	}

	/// Add a global event handler to the context.
	///
	/// Events that are already queued with the event loop will not be passed to the handler.
	///
	/// This function uses [`Self::run_function_wait`] internally, so it blocks until the event handler is added.
	/// To avoid blocking, you can use [`Self::run_function`] to post a lambda that adds an error handler instead.
	pub fn add_event_handler<F>(&mut self, handler: F) -> Result<(), EventLoopClosedError>
	where
		F: FnMut(ContextHandle<UserEvent>, &mut Event<UserEvent>) -> EventHandlerOutput + Send + 'static,
	{
		self.run_function_wait(move |context| {
			context.add_event_handler(handler)
		})
	}

	/// Add an event handler for a specific window.
	///
	/// Events that are already queued with the event loop will not be passed to the handler.
	///
	/// This function uses [`Self::run_function_wait`] internally, so it blocks until the event handler is added.
	/// To avoid blocking, you can use [`Self::run_function`] to post a lambda that adds an error handler instead.
	pub fn add_window_event_handler<F>(&mut self, window_id: WindowId, handler: F) -> Result<(), ProxyWindowOperationError>
	where
		F: FnMut(WindowHandle<UserEvent>, &mut WindowEvent) -> EventHandlerOutput + Send + 'static,
	{
		self.run_function_wait(move |context| {
			context.add_window_event_handler(window_id, handler)
		})??;
		Ok(())
	}

	/// Post a function for execution in the context thread without waiting for it to execute.
	///
	/// This function returns immediately, without waiting for the posted function to start or complete.
	/// If you want to get a return value back from the function, use [`Self::run_function_wait`] instead.
	///
	/// *Note:*
	/// You should not post functions to the context thread that block for a long time.
	/// Doing so will block the event loop and will make the windows unresponsive until the event loop can continue.
	pub fn run_function<F>(&self, function: F) -> Result<(), EventLoopClosedError>
	where
		F: 'static + FnOnce(&mut ContextHandle<UserEvent>) + Send,
	{
		let function = Box::new(function);
		let event = ExecuteFunction { function }.into();
		self.event_loop.send_event(event).map_err(|_| EventLoopClosedError)
	}

	/// Post a function for execution in the context thread and wait for the return value.
	///
	/// If you do not need a return value from the posted function,
	/// you can use [`Self::run_function`] to avoid blocking it completes.
	///
	/// *Note:*
	/// You should not post functions to the context thread that block for a long time.
	/// Doing so will block the event loop and will make the windows unresponsive until the event loop can continue.
	pub fn run_function_wait<F, T>(&self, function: F) -> Result<T, EventLoopClosedError>
	where
		F: FnOnce(&mut ContextHandle<UserEvent>) -> T + Send + 'static,
		T: Send + 'static,
	{
		let (result_tx, result_rx) = oneshot::channel();
		self.run_function(move |context| {
			result_tx.send((function)(context))
		})?;
		result_rx.recv().map_err(|_| EventLoopClosedError)
	}

	/// Send a user event to the context.
	pub fn send_user_event(&self, event: UserEvent) -> Result<(), EventLoopClosedError> {
		self.event_loop.send_event(ContextEvent::UserEvent(event)).map_err(|_| EventLoopClosedError)
	}
}

impl<UserEvent: 'static> WindowProxy<UserEvent> {
	/// Create a new window proxy from a context proxy and a window ID.
	pub fn new(window_id: WindowId, context_proxy: ContextProxy<UserEvent>) -> Self {
		Self { window_id, context_proxy }
	}

	/// Get the window ID.
	pub fn id(&self) -> WindowId {
		self.window_id
	}

	/// Get the context proxy of the window proxy.
	pub fn context_proxy(&self) -> &ContextProxy<UserEvent> {
		&self.context_proxy
	}

	/// Destroy the window.
	pub fn destroy(&self) -> Result<(), ProxyWindowOperationError> {
		self.context_proxy.destroy_window(self.window_id)
	}

	/// Set the image of the window.
	pub fn set_visible(
		&self,
		visible: bool,
	) -> Result<(), ProxyWindowOperationError> {
		self.context_proxy.set_window_visible(self.window_id, visible)
	}

	/// Set the image of the window.
	pub fn set_image(
		&self,
		name: impl Into<String>,
		image: Image<'static>,
	) -> Result<(), ProxyWindowOperationError> {
		self.context_proxy.set_window_image(self.window_id, name, image)
	}

	/// Add an event handler for a specific window.
	///
	/// Events that are already queued with the event loop will not be passed to the handler.
	///
	/// This function uses [`ContextHandle::run_function_wait`] internally, so it blocks until the event handler is added.
	/// To avoid blocking, you can use [`ContextHandle::run_function`] to post a lambda that adds an error handler instead.
	pub fn add_window_event_handler<F>(&mut self, handler: F) -> Result<(), ProxyWindowOperationError>
	where
		F: FnMut(WindowHandle<UserEvent>, &mut WindowEvent) -> EventHandlerOutput + Send + 'static,
	{
		self.context_proxy.add_window_event_handler(self.window_id, handler)
	}
}
