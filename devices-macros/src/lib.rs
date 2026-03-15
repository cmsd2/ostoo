use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::{parse_macro_input, ImplItem, ImplItemFn, ItemImpl, LitStr, Token, Type};

// ---------------------------------------------------------------------------
// #[on_message] — helper attribute consumed by #[actor].

/// Marker attribute for message handlers inside an [`actor`] impl block.
///
/// ```ignore
/// #[on_message(VariantName)]
/// async fn my_handler(&self, field: Type) { ... }
/// ```
///
/// `#[actor]` collects all `#[on_message]` methods, moves them to an inherent
/// impl, and generates the actor's `run` loop that dispatches to them.
/// This attribute has no effect when used outside an `#[actor]` block.
#[proc_macro_attribute]
pub fn on_message(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}

/// Marks a method as the periodic tick handler inside an [`actor`] impl block.
///
/// ```ignore
/// fn tick_interval_ticks(&self) -> u64 { 1000 }
///
/// #[on_tick]
/// async fn heartbeat(&self) { log::info!("tick"); }
/// ```
///
/// When `#[on_tick]` is present `#[actor]` includes a `Delay` in the unified
/// `poll_fn` run loop and calls the annotated method on every elapsed interval.
/// The actor struct must also provide a plain `tick_interval_ticks(&self) -> u64`
/// method (no attribute needed).
/// This attribute has no effect when used outside an `#[actor]` block.
#[proc_macro_attribute]
pub fn on_tick(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}

/// Override the default `ActorMsg::Info` handler inside an [`actor`] impl block.
///
/// Without this attribute every actor responds to `Info` with
/// `ActorInfo { name }` automatically.  Annotate one method with `#[on_info]`
/// to supply custom behaviour instead:
///
/// ```ignore
/// #[on_info]
/// async fn on_info(&self) -> MyInfo {
///     MyInfo { /* actor-specific fields */ }
/// }
/// ```
///
/// The method takes no arguments beyond `&self` and returns an actor-specific
/// info struct (which must implement [`core::fmt::Debug`] so it can be boxed
/// into [`ErasedInfo`] for type-erased registry queries).
/// The macro generates the `reply.send(...)` call automatically.
/// This attribute has no effect when used outside an `#[actor]` block.
#[proc_macro_attribute]
pub fn on_info(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}

/// Marks a method to be called once when the actor's run loop starts,
/// before the first message is received.
///
/// ```ignore
/// #[on_start]
/// async fn on_start(&self) {
///     log::info!("actor started");
/// }
/// ```
///
/// Only one `#[on_start]` method is allowed per actor.
/// This attribute has no effect when used outside an `#[actor]` block.
#[proc_macro_attribute]
pub fn on_start(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}

/// Registers an async `Stream` event source inside an [`actor`] impl block.
///
/// ```ignore
/// // A plain method returning the stream — called once when the actor starts.
/// fn my_stream(&self) -> impl Stream<Item = MyItem> + Unpin { ... }
///
/// // Handler called for each item produced by that stream.
/// #[on_stream(my_stream)]
/// async fn on_my_item(&self, item: MyItem) { ... }
/// ```
///
/// `#[actor]` includes the stream in the unified `poll_fn` run loop alongside
/// the inbox.  The stream factory method lands in the inherent impl unchanged;
/// the handler method has its `#[on_stream]` attribute stripped.
/// This attribute has no effect when used outside an `#[actor]` block.
#[proc_macro_attribute]
pub fn on_stream(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}

// ---------------------------------------------------------------------------
// #[actor] — the main actor macro.

struct ActorArgs {
    driver_name: LitStr,
    msg_type:    Type,
}

impl Parse for ActorArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let driver_name: LitStr = input.parse()?;
        input.parse::<Token![,]>()?;
        let msg_type: Type = input.parse()?;
        Ok(ActorArgs { driver_name, msg_type })
    }
}

/// Attribute macro that generates a full [`DriverTask`] implementation from an
/// annotated `impl` block.
///
/// # What it generates
/// - An inherent `impl StructName` containing all handler methods (attributes
///   stripped) and all other items from the block.
/// - `impl DriverTask for StructName` with `type Message`, `fn name`, and a
///   generated `run` loop.
/// - `pub type StructNameDriver = TaskDriver<StructName>`
///
/// # The generated `run` loop
///
/// **Pure message actors** (no `#[on_tick]` or `#[on_stream]`):
/// ```ignore
/// while let Some(msg) = inbox.recv().await { match msg { ... } }
/// ```
///
/// **Actors with external event sources** (`#[on_tick]` and/or `#[on_stream]`):
/// A unified `poll_fn` loop is generated that races all sources — streams,
/// the inbox, and the timer (if `#[on_tick]` is present) — in a single
/// `poll_fn` call.  Whichever source fires first drives the next dispatch.
///
/// # Usage
/// ```ignore
/// pub enum MyMsg { DoThing(u32) }
/// pub struct MyActor;
///
/// #[actor("my-actor", MyMsg)]
/// impl MyActor {
///     #[on_message(DoThing)]
///     async fn do_thing(&self, n: u32) { log::info!("doing thing {}", n); }
/// }
/// ```
#[proc_macro_attribute]
pub fn actor(attr: TokenStream, item: TokenStream) -> TokenStream {
    let ActorArgs { driver_name, msg_type } = parse_macro_input!(attr as ActorArgs);
    let input = parse_macro_input!(item as ItemImpl);

    match actor_expand(driver_name, msg_type, input) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

fn actor_expand(
    driver_name: LitStr,
    msg_type:    Type,
    input:       ItemImpl,
) -> syn::Result<TokenStream2> {
    if let Some((_, path, _)) = &input.trait_ {
        return Err(syn::Error::new_spanned(
            path,
            "#[actor] must be on `impl Type { }`, not `impl Trait for Type { }`",
        ));
    }

    let self_ty = &input.self_ty;

    let driver_alias = match self_ty.as_ref() {
        syn::Type::Path(tp) => {
            let ident = &tp.path.segments.last().unwrap().ident;
            format_ident!("{}Driver", ident)
        }
        _ => return Err(syn::Error::new_spanned(self_ty, "#[actor] requires a named type")),
    };

    // Separate items into their categories.
    let mut handler_methods: Vec<HandlerMethod> = Vec::new();
    let mut info_override:   Option<InfoOverride> = None;
    let mut on_start:        Option<OnStart>       = None;
    let mut on_tick:         Option<OnTick>        = None;
    let mut on_streams:      Vec<OnStream>          = Vec::new();
    let mut other_items:     Vec<&ImplItem>         = Vec::new();

    for item in &input.items {
        if let ImplItem::Fn(method) = item {
            if let Some(variant) = on_message_variant(method)? {
                handler_methods.push(HandlerMethod::new(method, variant)?);
                continue;
            }
            if has_on_info(method) {
                if info_override.is_some() {
                    return Err(syn::Error::new_spanned(
                        method, "only one #[on_info] method is allowed per actor",
                    ));
                }
                info_override = Some(InfoOverride::new(method)?);
                continue;
            }
            if has_on_start(method) {
                if on_start.is_some() {
                    return Err(syn::Error::new_spanned(
                        method, "only one #[on_start] method is allowed per actor",
                    ));
                }
                on_start = Some(OnStart::new(method)?);
                continue;
            }
            if has_on_tick(method) {
                if on_tick.is_some() {
                    return Err(syn::Error::new_spanned(
                        method, "only one #[on_tick] method is allowed per actor",
                    ));
                }
                on_tick = Some(OnTick::new(method)?);
                continue;
            }
            if has_on_stream(method) {
                on_streams.push(OnStream::new(method)?);
                continue;
            }
        }
        other_items.push(item);
    }

    // Build match arms for inner message dispatch.
    let inner_arms = handler_methods.iter().map(|h| {
        let variant     = &h.variant;
        let method_name = &h.method_name;
        let params      = &h.params;
        let pattern = if params.is_empty() {
            quote! { #msg_type::#variant }
        } else {
            quote! { #msg_type::#variant(#(#params),*) }
        };
        quote! { #pattern => handle.#method_name(#(#params),*).await, }
    });

    // Compute Info detail type.
    let info_type: syn::Type = if let Some(ref ov) = info_override {
        ov.info_type.clone()
    } else {
        syn::parse_quote! { () }
    };

    // Typed Info arm.
    let info_arm = if let Some(ref ov) = info_override {
        let method_name = &ov.method_name;
        quote! {
            ::libkernel::task::mailbox::ActorMsg::Info(reply) => {
                reply.send(::libkernel::task::mailbox::ActorStatus {
                    name: #driver_name, running: true,
                    info: handle.#method_name().await,
                });
            }
        }
    } else {
        quote! {
            ::libkernel::task::mailbox::ActorMsg::Info(reply) => {
                reply.send(::libkernel::task::mailbox::ActorStatus {
                    name: #driver_name, running: true, info: (),
                });
            }
        }
    };

    // Type-erased ErasedInfo arm.
    let erased_info_arm = if let Some(ref ov) = info_override {
        let method_name = &ov.method_name;
        quote! {
            ::libkernel::task::mailbox::ActorMsg::ErasedInfo(reply) => {
                let info: ::alloc::boxed::Box<dyn ::core::fmt::Debug + Send> =
                    ::alloc::boxed::Box::new(handle.#method_name().await);
                reply.send(::libkernel::task::mailbox::ActorStatus {
                    name: #driver_name, running: true, info,
                });
            }
        }
    } else {
        quote! {
            ::libkernel::task::mailbox::ActorMsg::ErasedInfo(reply) => {
                reply.send(::libkernel::task::mailbox::ActorStatus {
                    name: #driver_name, running: true,
                    info: ::alloc::boxed::Box::new(""),
                });
            }
        }
    };

    // All extracted methods + other items go into the inherent impl.
    let inherent_fns = handler_methods.iter().map(|h| &h.clean_method)
        .chain(info_override.iter().map(|ov| &ov.clean_method))
        .chain(on_start.iter().map(|os| &os.clean_method))
        .chain(on_tick.iter().map(|ot| &ot.clean_method))
        .chain(on_streams.iter().map(|os| &os.clean_method));

    // Optional startup call emitted once before the run loop.
    let start_call: TokenStream2 = if let Some(ref os) = on_start {
        let method = &os.method_name;
        quote! { handle.#method().await; }
    } else {
        quote! {}
    };

    let name_str = driver_name.value();

    // ── Run loop generation ────────────────────────────────────────────────
    //
    // Pure message actors → simple `while let Some(msg) = inbox.recv().await`.
    //
    // Actors with external event sources (streams and/or tick) → a unified
    // `poll_fn` loop that races all sources in one future.  The wakers for
    // every source (mailbox AtomicWaker, stream AtomicWaker, timer WAKERS)
    // all point at the same task, so whichever fires first reschedules it.
    // ──────────────────────────────────────────────────────────────────────

    let has_external = !on_streams.is_empty() || on_tick.is_some();

    let run_loop = if !has_external {
        // ── Simple loop ────────────────────────────────────────────────────
        quote! {
            while let ::core::option::Option::Some(_msg) = inbox.recv().await {
                match _msg {
                    #info_arm
                    #erased_info_arm
                    ::libkernel::task::mailbox::ActorMsg::Inner(_msg) => match _msg {
                        #(#inner_arms)*
                    }
                }
            }
        }
    } else {
        // ── Unified poll_fn loop ────────────────────────────────────────────

        // One local per stream, initialised once before the loop.
        let stream_locals: Vec<TokenStream2> = on_streams.iter().enumerate()
            .map(|(i, s)| {
                let factory = &s.factory;
                let var     = format_ident!("_stream_{}", i);
                quote! { let mut #var = handle.#factory(); }
            })
            .collect();

        // Delay local for tick (reset after each tick event).
        let delay_init: TokenStream2 = if on_tick.is_some() {
            quote! {
                let mut _delay =
                    ::libkernel::task::timer::Delay::new(handle.tick_interval_ticks());
            }
        } else {
            quote! {}
        };

        // _Event enum variants: one per stream, optional Tick, always Stopped.
        let stream_variants: Vec<TokenStream2> = on_streams.iter().enumerate()
            .map(|(i, s)| {
                let variant = format_ident!("_Stream{}", i);
                let ty      = &s.param_ty;
                quote! { #variant(#ty), }
            })
            .collect();
        let tick_variant: TokenStream2 = if on_tick.is_some() {
            quote! { _Tick, }
        } else {
            quote! {}
        };

        // poll_fn arms: streams polled first (interrupt-driven, fast path).
        let stream_poll_arms: Vec<TokenStream2> = on_streams.iter().enumerate()
            .map(|(i, _)| {
                let var     = format_ident!("_stream_{}", i);
                let variant = format_ident!("_Stream{}", i);
                quote! {
                    match ::libkernel::task::poll_stream_next(&mut #var, cx) {
                        ::core::task::Poll::Ready(::core::option::Option::Some(_item)) =>
                            return ::core::task::Poll::Ready(_Event::#variant(_item)),
                        ::core::task::Poll::Ready(::core::option::Option::None) =>
                            return ::core::task::Poll::Ready(_Event::_Stopped),
                        ::core::task::Poll::Pending => {}
                    }
                }
            })
            .collect();
        let tick_poll_arm: TokenStream2 = if on_tick.is_some() {
            quote! {
                if let ::core::task::Poll::Ready(()) =
                    ::core::pin::Pin::new(&mut _delay).poll(cx)
                {
                    return ::core::task::Poll::Ready(_Event::_Tick);
                }
            }
        } else {
            quote! {}
        };

        // match arms in the event loop.
        let stream_match_arms: Vec<TokenStream2> = on_streams.iter().enumerate()
            .map(|(i, s)| {
                let variant = format_ident!("_Stream{}", i);
                let method  = &s.method_name;
                let param   = &s.param;
                quote! { _Event::#variant(#param) => handle.#method(#param).await, }
            })
            .collect();
        let tick_match_arm: TokenStream2 = if let Some(ref ot) = on_tick {
            let method = &ot.method_name;
            quote! {
                _Event::_Tick => {
                    handle.#method().await;
                    _delay = ::libkernel::task::timer::Delay::new(
                        handle.tick_interval_ticks()
                    );
                }
            }
        } else {
            quote! {}
        };

        quote! {
            #(#stream_locals)*
            #delay_init
            loop {
                // Local enum: _Inbox, one _StreamN per source, optional _Tick,
                // _Stopped.  Defined inside the loop so types are always fresh.
                enum _Event {
                    _Inbox(::libkernel::task::mailbox::ActorMsg<#msg_type, #info_type>),
                    #(#stream_variants)*
                    #tick_variant
                    _Stopped,
                }
                let mut _recv = inbox.recv();
                let _ev = ::core::future::poll_fn(|cx| {
                    // Streams first — interrupt-driven, expected to be lowest
                    // latency.
                    #(#stream_poll_arms)*
                    // Inbox — control messages and stop signal.
                    match ::core::pin::Pin::new(&mut _recv).poll(cx) {
                        ::core::task::Poll::Ready(::core::option::Option::Some(_msg)) =>
                            return ::core::task::Poll::Ready(_Event::_Inbox(_msg)),
                        ::core::task::Poll::Ready(::core::option::Option::None) =>
                            return ::core::task::Poll::Ready(_Event::_Stopped),
                        ::core::task::Poll::Pending => {}
                    }
                    // Tick timer (lowest priority).
                    #tick_poll_arm
                    ::core::task::Poll::Pending
                }).await;
                match _ev {
                    _Event::_Stopped => break,
                    _Event::_Inbox(_msg) => match _msg {
                        #info_arm
                        #erased_info_arm
                        ::libkernel::task::mailbox::ActorMsg::Inner(_msg) => match _msg {
                            #(#inner_arms)*
                        }
                    }
                    #(#stream_match_arms)*
                    #tick_match_arm
                }
            }
        }
    };

    Ok(quote! {
        impl #self_ty {
            #(#inherent_fns)*
            #(#other_items)*
        }

        impl crate::task_driver::DriverTask for #self_ty {
            type Message = #msg_type;
            type Info    = #info_type;

            fn name(&self) -> &'static str { #driver_name }

            async fn run(
                handle: ::alloc::sync::Arc<Self>,
                _stop:  crate::task_driver::StopToken,
                inbox:  ::alloc::sync::Arc<
                    ::libkernel::task::mailbox::Mailbox<
                        ::libkernel::task::mailbox::ActorMsg<#msg_type, #info_type>
                    >
                >,
            ) {
                #[allow(unused_imports)]
                use ::core::future::Future as _;
                ::log::info!("[{}] started", #name_str);
                #start_call
                #run_loop
                ::log::info!("[{}] stopped", #name_str);
            }
        }

        pub type #driver_alias = crate::task_driver::TaskDriver<#self_ty>;
    })
}

// ---------------------------------------------------------------------------
// Helpers

struct HandlerMethod {
    variant:      syn::Ident,
    method_name:  syn::Ident,
    params:       Vec<syn::Ident>,
    clean_method: ImplItemFn,
}

impl HandlerMethod {
    fn new(method: &ImplItemFn, variant: syn::Ident) -> syn::Result<Self> {
        let method_name = method.sig.ident.clone();
        let params = method.sig.inputs.iter()
            .filter_map(|arg| match arg {
                syn::FnArg::Receiver(_) => None,
                syn::FnArg::Typed(pt)   => match pt.pat.as_ref() {
                    syn::Pat::Ident(pi) => Some(pi.ident.clone()),
                    other => Some(syn::Ident::new(
                        &format!("_param{}", quote!(#other)),
                        proc_macro2::Span::call_site(),
                    )),
                },
            })
            .collect();

        let mut clean = method.clone();
        clean.attrs.retain(|a| !a.path().is_ident("on_message"));

        Ok(HandlerMethod { variant, method_name, params, clean_method: clean })
    }
}

fn on_message_variant(method: &ImplItemFn) -> syn::Result<Option<syn::Ident>> {
    for attr in &method.attrs {
        if attr.path().is_ident("on_message") {
            let variant: syn::Ident = attr.parse_args()?;
            return Ok(Some(variant));
        }
    }
    Ok(None)
}

fn has_on_info(method: &ImplItemFn) -> bool {
    method.attrs.iter().any(|a| a.path().is_ident("on_info"))
}

struct InfoOverride {
    method_name:  syn::Ident,
    clean_method: ImplItemFn,
    info_type:    syn::Type,
}

impl InfoOverride {
    fn new(method: &ImplItemFn) -> syn::Result<Self> {
        let method_name = method.sig.ident.clone();
        let info_type = match &method.sig.output {
            syn::ReturnType::Type(_, ty) => *ty.clone(),
            syn::ReturnType::Default => {
                return Err(syn::Error::new_spanned(
                    method,
                    "#[on_info] method must return an info type, e.g. `-> MyInfo`",
                ));
            }
        };
        let mut clean = method.clone();
        clean.attrs.retain(|a| !a.path().is_ident("on_info"));
        Ok(InfoOverride { method_name, clean_method: clean, info_type })
    }
}

struct OnStart {
    method_name:  syn::Ident,
    clean_method: ImplItemFn,
}

impl OnStart {
    fn new(method: &ImplItemFn) -> syn::Result<Self> {
        let method_name = method.sig.ident.clone();
        let mut clean = method.clone();
        clean.attrs.retain(|a| !a.path().is_ident("on_start"));
        Ok(OnStart { method_name, clean_method: clean })
    }
}

fn has_on_start(method: &ImplItemFn) -> bool {
    method.attrs.iter().any(|a| a.path().is_ident("on_start"))
}

struct OnTick {
    method_name:  syn::Ident,
    clean_method: ImplItemFn,
}

impl OnTick {
    fn new(method: &ImplItemFn) -> syn::Result<Self> {
        let method_name = method.sig.ident.clone();
        let mut clean = method.clone();
        clean.attrs.retain(|a| !a.path().is_ident("on_tick"));
        Ok(OnTick { method_name, clean_method: clean })
    }
}

fn has_on_tick(method: &ImplItemFn) -> bool {
    method.attrs.iter().any(|a| a.path().is_ident("on_tick"))
}

struct OnStream {
    factory:      syn::Ident,
    method_name:  syn::Ident,
    param:        syn::Ident,
    param_ty:     syn::Type,
    clean_method: ImplItemFn,
}

impl OnStream {
    fn new(method: &ImplItemFn) -> syn::Result<Self> {
        let factory = method.attrs.iter()
            .find(|a| a.path().is_ident("on_stream"))
            .unwrap()
            .parse_args::<syn::Ident>()?;

        let method_name = method.sig.ident.clone();

        let param_arg = method.sig.inputs.iter()
            .filter_map(|a| match a { syn::FnArg::Typed(pt) => Some(pt), _ => None })
            .next()
            .ok_or_else(|| syn::Error::new_spanned(
                method,
                "#[on_stream] handler must take exactly one non-self parameter",
            ))?;

        let param = match param_arg.pat.as_ref() {
            syn::Pat::Ident(pi) => pi.ident.clone(),
            other => return Err(syn::Error::new_spanned(
                other,
                "#[on_stream] parameter must be a simple identifier",
            )),
        };

        let param_ty = *param_arg.ty.clone();

        let mut clean = method.clone();
        clean.attrs.retain(|a| !a.path().is_ident("on_stream"));

        Ok(OnStream { factory, method_name, param, param_ty, clean_method: clean })
    }
}

fn has_on_stream(method: &ImplItemFn) -> bool {
    method.attrs.iter().any(|a| a.path().is_ident("on_stream"))
}
