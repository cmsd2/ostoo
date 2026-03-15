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
/// - `ActorMsg::Info(reply)` → replies with `ActorInfo { name }` automatically.
/// - `ActorMsg::Inner(m)` → dispatches `m` to the corresponding handler.
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

    // Separate items: methods with #[on_message] become handlers; everything
    // else stays in the DriverTask impl.
    let mut handler_methods: Vec<HandlerMethod> = Vec::new();
    let mut trait_items:     Vec<&ImplItem>     = Vec::new();

    for item in &input.items {
        if let ImplItem::Fn(method) = item {
            if let Some(variant) = on_message_variant(method)? {
                handler_methods.push(HandlerMethod::new(method, variant)?);
                continue;
            }
        }
        trait_items.push(item);
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

    // Handler methods go into a plain inherent impl (not the trait impl).
    let inherent_fns = handler_methods.iter().map(|h| &h.clean_method);

    let name_str = driver_name.value();

    Ok(quote! {
        impl #self_ty {
            #(#inherent_fns)*
        }

        impl crate::task_driver::DriverTask for #self_ty {
            type Message = #msg_type;

            fn name(&self) -> &'static str { #driver_name }

            #(#trait_items)*

            async fn run(
                handle: ::alloc::sync::Arc<Self>,
                _stop:  crate::task_driver::StopToken,
                inbox:  ::alloc::sync::Arc<
                    ::libkernel::task::mailbox::Mailbox<
                        ::libkernel::task::mailbox::ActorMsg<#msg_type>
                    >
                >,
            ) {
                ::log::info!("[{}] started", #name_str);
                while let ::core::option::Option::Some(_msg) = inbox.recv().await {
                    match _msg {
                        ::libkernel::task::mailbox::ActorMsg::Info(reply) => {
                            reply.send(::libkernel::task::mailbox::ActorInfo {
                                name: #driver_name,
                            });
                        }
                        ::libkernel::task::mailbox::ActorMsg::Inner(_msg) => match _msg {
                            #(#inner_arms)*
                        }
                    }
                }
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
