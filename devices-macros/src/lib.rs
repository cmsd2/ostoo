use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::{parse_macro_input, ImplItem, ImplItemFn, ItemImpl, LitStr, Token, Type};

// ---------------------------------------------------------------------------
// #[on_message] — helper attribute consumed by #[actor].
//
// Registering it as a proc macro makes it a recognised attribute, giving
// better IDE support and a clear error if used outside an #[actor] block.

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
/// When `#[on_tick]` is present `#[actor]` replaces `inbox.recv()` with
/// `inbox.recv_timeout(handle.tick_interval_ticks())` and calls the annotated
/// method on every elapsed interval.  The actor struct must also provide a
/// plain `tick_interval_ticks(&self) -> u64` method (no attribute needed).
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
/// - An inherent `impl StructName` containing all methods annotated with
///   [`on_message`], with the `#[on_message]` attribute stripped.
/// - `impl DriverTask for StructName` with `type Message`, `fn name`, and a
///   generated `run` loop that handles both the generic `ActorMsg::Info`
///   variant and actor-specific `ActorMsg::Inner(m)` variants.
/// - `pub type StructNameDriver = TaskDriver<StructName>`
///
/// # The generated `run` loop
/// Calls `inbox.recv().await` in a loop and dispatches each message:
/// - `ActorMsg::Info(reply)` → replies with `ActorStatus { name, running: true, info }`;
///   `info` is `()` by default, or the return value of the `#[on_info]` method.
/// - `ActorMsg::ErasedInfo(reply)` → same but boxes `info` as `Box<dyn Debug + Send>`.
/// - `ActorMsg::Inner(m)` → dispatches `m` to the corresponding `#[on_message]` handler.
/// The loop exits when the inbox is closed (i.e. when [`Driver::stop`] is
/// called on the owning [`TaskDriver`]).
///
/// # Usage
/// ```ignore
/// pub enum MyMsg { DoThing(u32) }
/// pub struct MyActor;
///
/// #[actor("my-actor", MyMsg)]
/// impl MyActor {
///     #[on_message(DoThing)]
///     async fn do_thing(&self, n: u32) {
///         log::info!("doing thing {}", n);
///     }
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

    // Separate items: methods with #[on_message] become inner handlers;
    // a method with #[on_info] overrides the Info arm; a method with
    // #[on_tick] becomes the periodic tick handler; everything else goes
    // into the inherent impl unchanged.
    let mut handler_methods: Vec<HandlerMethod> = Vec::new();
    let mut info_override:   Option<InfoOverride> = None;
    let mut on_tick:         Option<OnTick>        = None;
    let mut other_items:     Vec<&ImplItem>        = Vec::new();

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
            if has_on_tick(method) {
                if on_tick.is_some() {
                    return Err(syn::Error::new_spanned(
                        method, "only one #[on_tick] method is allowed per actor",
                    ));
                }
                on_tick = Some(OnTick::new(method)?);
                continue;
            }
        }
        other_items.push(item);
    }

    // Build match arms for the inner message dispatch.
    let inner_arms = handler_methods.iter().map(|h| {
        let variant     = &h.variant;
        let method_name = &h.method_name;
        let params      = &h.params;
        let pattern = if params.is_empty() {
            quote! { #msg_type::#variant }
        } else {
            quote! { #msg_type::#variant(#(#params),*) }
        };
        quote! {
            #pattern => handle.#method_name(#(#params),*).await,
        }
    });

    // Compute the Info detail type from the #[on_info] return type, or ().
    let info_type: syn::Type = if let Some(ref ov) = info_override {
        ov.info_type.clone()
    } else {
        syn::parse_quote! { () }
    };

    // Typed Info arm: returns ActorStatus<I> with the full detail.
    let info_arm = if let Some(ref ov) = info_override {
        let method_name = &ov.method_name;
        quote! {
            ::libkernel::task::mailbox::ActorMsg::Info(reply) => {
                reply.send(::libkernel::task::mailbox::ActorStatus {
                    name: #driver_name,
                    running: true,
                    info: handle.#method_name().await,
                });
            }
        }
    } else {
        quote! {
            ::libkernel::task::mailbox::ActorMsg::Info(reply) => {
                reply.send(::libkernel::task::mailbox::ActorStatus {
                    name: #driver_name,
                    running: true,
                    info: (),
                });
            }
        }
    };

    // Type-erased ErasedInfo arm: boxes the info behind dyn Debug + Send.
    // Actors with #[on_info] box their typed detail (requires Debug + Send).
    // Actors without #[on_info] box () as a placeholder.
    let erased_info_arm = if let Some(ref ov) = info_override {
        let method_name = &ov.method_name;
        quote! {
            ::libkernel::task::mailbox::ActorMsg::ErasedInfo(reply) => {
                let info: ::alloc::boxed::Box<dyn ::core::fmt::Debug + Send> =
                    ::alloc::boxed::Box::new(handle.#method_name().await);
                reply.send(::libkernel::task::mailbox::ActorStatus {
                    name: #driver_name,
                    running: true,
                    info,
                });
            }
        }
    } else {
        quote! {
            ::libkernel::task::mailbox::ActorMsg::ErasedInfo(reply) => {
                reply.send(::libkernel::task::mailbox::ActorStatus {
                    name: #driver_name,
                    running: true,
                    info: ::alloc::boxed::Box::new(""),
                });
            }
        }
    };

    // All extracted methods + plain other_items go into the inherent impl.
    let inherent_fns = handler_methods.iter().map(|h| &h.clean_method)
        .chain(info_override.iter().map(|ov| &ov.clean_method))
        .chain(on_tick.iter().map(|ot| &ot.clean_method));

    let name_str = driver_name.value();

    // Generate the run loop body — with or without tick support.
    let run_loop = if let Some(ref ot) = on_tick {
        let tick_method = &ot.method_name;
        quote! {
            loop {
                match inbox.recv_timeout(handle.tick_interval_ticks()).await {
                    ::libkernel::task::mailbox::RecvTimeout::Message(_msg) => match _msg {
                        #info_arm
                        #erased_info_arm
                        ::libkernel::task::mailbox::ActorMsg::Inner(_msg) => match _msg {
                            #(#inner_arms)*
                        }
                    }
                    ::libkernel::task::mailbox::RecvTimeout::Closed => break,
                    ::libkernel::task::mailbox::RecvTimeout::Elapsed => {
                        handle.#tick_method().await;
                    }
                }
            }
        }
    } else {
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
                ::log::info!("[{}] started", #name_str);
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

        // Strip #[on_message] from the method before placing it in the
        // inherent impl so the compiler doesn't see an unknown attribute.
        let mut clean = method.clone();
        clean.attrs.retain(|a| !a.path().is_ident("on_message"));

        Ok(HandlerMethod { variant, method_name, params, clean_method: clean })
    }
}

/// Extract the variant ident from `#[on_message(VariantName)]`, or return
/// `None` if the method has no such attribute.
fn on_message_variant(method: &ImplItemFn) -> syn::Result<Option<syn::Ident>> {
    for attr in &method.attrs {
        if attr.path().is_ident("on_message") {
            let variant: syn::Ident = attr.parse_args()?;
            return Ok(Some(variant));
        }
    }
    Ok(None)
}

/// Return `true` if the method has `#[on_info]`.
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
