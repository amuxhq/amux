//! The shapes every ACP message is built from, and the strictness switch
//! they share.
//!
//! Each shape tolerates what this crate does not know and keeps it, so a
//! line decodes and encodes back to the same JSON. In strict mode the same
//! fallbacks fail instead, which is how the recording checks see drift at
//! any depth of a message.

use std::cell::Cell;

thread_local! {
    static STRICT: Cell<bool> = const { Cell::new(false) };
}

/// Runs `decode` with every fallback turned into an error.
pub(crate) fn strictly<T>(decode: impl FnOnce() -> T) -> T {
    struct Reset(bool);
    impl Drop for Reset {
        fn drop(&mut self) {
            STRICT.with(|strict| strict.set(self.0));
        }
    }
    let _reset = Reset(STRICT.with(|strict| strict.replace(true)));
    decode()
}

pub(crate) fn is_strict() -> bool {
    STRICT.with(Cell::get)
}

/// A string-valued enum: each known spelling is a variant and any other is
/// `Other`, kept as written.
macro_rules! string_enum {
    (
        $(#[$meta:meta])*
        pub enum $name:ident { $($(#[$vmeta:meta])* $variant:ident = $wire:literal,)* }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        pub enum $name {
            $($(#[$vmeta])* $variant,)*
            /// A spelling this crate does not know.
            Other(String),
        }

        impl $name {
            /// Every spelling this crate knows.
            pub const SPELLINGS: &'static [&'static str] = &[$($wire,)*];

            pub fn as_str(&self) -> &str {
                match self {
                    $(Self::$variant => $wire,)*
                    Self::Other(other) => other,
                }
            }

            /// The variant written `text`; `Other` when none is.
            pub fn parse(text: &str) -> Self {
                match text {
                    $($wire => Self::$variant,)*
                    other => Self::Other(other.to_owned()),
                }
            }
        }

        impl ::serde::Serialize for $name {
            fn serialize<S: ::serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> ::serde::Deserialize<'de> for $name {
            fn deserialize<D: ::serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let text = String::deserialize(deserializer)?;
                Ok(match text.as_str() {
                    $($wire => Self::$variant,)*
                    _ if $crate::macros::is_strict() => {
                        return Err(::serde::de::Error::custom(format!(
                            concat!("unknown ", stringify!($name), " {:?}"),
                            text
                        )));
                    }
                    _ => Self::Other(text),
                })
            }
        }
    };
}

/// An object enum told apart by one string field (`type`, `sessionUpdate`,
/// `outcome`…): each known value is a variant holding the rest of the
/// object, and any other is `Unknown`, holding the whole object. ACP writes
/// some unions with one member that has no tag at all (an MCP server run
/// over stdio, an agent's own sign-in); that member follows `without`, and
/// an object lacking the field is read as it.
macro_rules! tagged_enum {
    (
        $(#[$meta:meta])*
        pub enum $name:ident by $field:literal {
            $($(#[$vmeta:meta])* $tag:literal => $variant:ident($payload:ty),)*
        }
        $($(#[$dmeta:meta])* without $default:ident($dpayload:ty))?
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq)]
        // A message is read and dropped one at a time, so a large member
        // is not worth boxing.
        #[allow(clippy::large_enum_variant)]
        pub enum $name {
            $($(#[$vmeta])* $variant($payload),)*
            $($(#[$dmeta])* $default($dpayload),)?
            /// A value this crate does not know, or a known one whose fields
            /// did not decode, as written.
            Unknown(::serde_json::Map<String, ::serde_json::Value>),
        }

        impl $name {
            /// Every value of the field this crate knows.
            pub const TAGS: &'static [&'static str] = &[$($tag,)*];

            /// The value of the field this object is told apart by; empty
            /// for a member written without it.
            pub fn kind(&self) -> &str {
                match self {
                    $(Self::$variant(_) => $tag,)*
                    $(Self::$default(_) => "",)?
                    Self::Unknown(object) => object.get($field).and_then(|kind| kind.as_str()).unwrap_or(""),
                }
            }
        }

        impl ::serde::Serialize for $name {
            fn serialize<S: ::serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                use ::serde::ser::Error as _;
                fn object<S: ::serde::Serializer>(
                    payload: &impl ::serde::Serialize,
                ) -> Result<::serde_json::Map<String, ::serde_json::Value>, S::Error> {
                    match ::serde_json::to_value(payload).map_err(S::Error::custom)? {
                        ::serde_json::Value::Object(object) => Ok(object),
                        _ => Err(S::Error::custom(concat!(stringify!($name), " is not an object"))),
                    }
                }
                let object = match self {
                    $(Self::$variant(payload) => {
                        let mut object = object::<S>(payload)?;
                        object.insert($field.into(), ::serde_json::Value::String($tag.into()));
                        object
                    })*
                    $(Self::$default(payload) => object::<S>(payload)?,)?
                    Self::Unknown(object) => object.clone(),
                };
                object.serialize(serializer)
            }
        }

        impl<'de> ::serde::Deserialize<'de> for $name {
            fn deserialize<D: ::serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                use ::serde::de::Error as _;
                let object = ::serde_json::Map::<String, ::serde_json::Value>::deserialize(deserializer)?;
                let kind = object.get($field).map(|kind| kind.as_str());
                let decoded = match kind {
                    Some(Some(kind)) => {
                        let mut fields = object.clone();
                        fields.remove($field);
                        match kind {
                            $($tag => ::serde_json::from_value(::serde_json::Value::Object(fields)).map(Self::$variant),)*
                            _ => Err(::serde_json::Error::custom(format!(
                                concat!("unknown ", stringify!($name), " ", $field, " {:?}"),
                                kind
                            ))),
                        }
                    }
                    Some(None) => Err(::serde_json::Error::custom(concat!($field, " is not a string"))),
                    None => $crate::macros::without_tag!($field, object.clone(), $($default($dpayload))?),
                };
                match decoded {
                    Ok(value) => Ok(value),
                    Err(error) if $crate::macros::is_strict() => Err(D::Error::custom(format!(
                        concat!(stringify!($name), " {:?}: {}"),
                        kind.flatten().unwrap_or(""), error
                    ))),
                    Err(_) => Ok(Self::Unknown(object)),
                }
            }
        }
    };
}

/// How a [`tagged_enum`] reads an object lacking its field: as its member
/// written without one, or not at all.
macro_rules! without_tag {
    ($field:literal, $object:expr,) => {
        Err(::serde_json::Error::custom(concat!("no ", $field)))
    };
    ($field:literal, $object:expr, $default:ident($payload:ty)) => {
        ::serde_json::from_value::<$payload>(::serde_json::Value::Object($object))
            .map(Self::$default)
    };
}

/// A JSON-RPC method enum: each known method is a variant holding its
/// params. A method this crate does not know has no variant; the envelope
/// keeps that whole line instead.
///
/// A request enum names the enum of the answers to it (`answered by`),
/// each method's answer type after `->`. The answers' own lines do not
/// say which request they answer, so they are read by the method of the
/// request their id names ([`read`](Self::read)), or, without it, as the
/// first answer type that reads them with nothing left unknown.
macro_rules! method_enum {
    (
        $(#[$meta:meta])*
        pub enum $name:ident answered by $answers:ident {
            $($(#[$vmeta:meta])* $method:literal => $variant:ident($payload:ty) -> $answer:ty,)*
        }
    ) => {
        $crate::macros::method_enum! {
            $(#[$meta])*
            pub enum $name { $($(#[$vmeta])* $method => $variant($payload),)* }
        }

        #[doc = concat!("An answer to a [`", stringify!($name), "`], by the method it answers.")]
        #[derive(Debug, Clone, PartialEq)]
        // A message is read and dropped one at a time, so a large member
        // is not worth boxing.
        #[allow(clippy::large_enum_variant)]
        pub enum $answers {
            $($variant($answer),)*
        }

        impl $answers {
            /// The method of the request this answers.
            pub fn method(&self) -> &'static str {
                match self {
                    $(Self::$variant(_) => $method,)*
                }
            }

            /// Reads `result` as the answer to a `method` request. None when
            /// the method is not one of these.
            pub fn read(
                method: &str,
                result: &::serde_json::Value,
            ) -> Option<Result<Self, ::serde_json::Error>> {
                use ::serde::Deserialize as _;
                Some(match method {
                    $($method => <$answer>::deserialize(result).map(Self::$variant),)*
                    _ => return None,
                })
            }

            /// [`read`](Self::read), failing where it would keep anything
            /// unknown.
            pub fn strict_read(
                method: &str,
                result: &::serde_json::Value,
            ) -> Option<Result<Self, ::serde_json::Error>> {
                $crate::macros::strictly(|| Self::read(method, result))
            }

            /// Reads `result` as each answer type in turn, so a probe hears
            /// every name any of them types.
            #[allow(dead_code)]
            pub(crate) fn read_as_each(result: &::serde_json::Value) {
                use ::serde::Deserialize as _;
                $(let _ = <$answer>::deserialize(result);)*
            }

            fn first(
                result: &::serde_json::Value,
            ) -> Result<Self, ::serde_json::Error> {
                use ::serde::Deserialize as _;
                use ::serde::de::Error as _;
                $(if let Ok(answer) = <$answer>::deserialize(result) {
                    return Ok(Self::$variant(answer));
                })*
                Err(::serde_json::Error::custom(concat!("not any ", stringify!($answers))))
            }
        }

        impl ::serde::Serialize for $answers {
            fn serialize<S: ::serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                match self {
                    $(Self::$variant(answer) => answer.serialize(serializer),)*
                }
            }
        }

        /// The first answer type that reads the value with nothing left
        /// unknown; failing that, outside strict mode, the first that reads
        /// it at all.
        impl<'de> ::serde::Deserialize<'de> for $answers {
            fn deserialize<D: ::serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                use ::serde::de::Error as _;
                let result = ::serde_json::Value::deserialize(deserializer)?;
                let exact = $crate::macros::strictly(|| Self::first(&result));
                match exact {
                    Ok(answer) => Ok(answer),
                    Err(error) if $crate::macros::is_strict() => Err(D::Error::custom(error)),
                    Err(_) => Self::first(&result).map_err(D::Error::custom),
                }
            }
        }
    };
    (
        $(#[$meta:meta])*
        pub enum $name:ident { $($(#[$vmeta:meta])* $method:literal => $variant:ident($payload:ty),)* }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq)]
        // A message is read and dropped one at a time, so a large member
        // is not worth boxing.
        #[allow(clippy::large_enum_variant)]
        pub enum $name {
            $($(#[$vmeta])* $variant($payload),)*
        }

        impl $name {
            /// Every method this enum types.
            pub const METHODS: &'static [&'static str] = &[$($method,)*];

            /// The method this message is sent as.
            pub fn method(&self) -> &'static str {
                match self {
                    $(Self::$variant(_) => $method,)*
                }
            }

            /// None when the method is not one of these. A line without
            /// params is read as one with empty params.
            pub(crate) fn from_parts(
                method: &str,
                params: ::serde_json::Value,
            ) -> Option<Result<Self, ::serde_json::Error>> {
                let params = match params {
                    ::serde_json::Value::Null => ::serde_json::Value::Object(::serde_json::Map::new()),
                    params => params,
                };
                Some(match method {
                    $($method => ::serde_json::from_value(params).map(Self::$variant),)*
                    _ => return None,
                })
            }

            /// The params as written.
            pub(crate) fn params(&self) -> ::serde_json::Value {
                match self {
                    $(Self::$variant(params) => ::serde_json::to_value(params).expect("params serialize"),)*
                }
            }
        }
    };
}

pub(crate) use method_enum;
pub(crate) use string_enum;
pub(crate) use tagged_enum;
pub(crate) use without_tag;
