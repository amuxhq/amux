# Keep stack-trace line numbers so R8 mapping.txt can de-obfuscate release reports.
-keepattributes SourceFile,LineNumberTable
-renamesourcefileattribute SourceFile

# Rust JNI entry points looked up by exact class and method name from crates/app-jni.
-keep class sh.amux.core.runtime.NativeBridge {
    native <methods>;
}

# Exception thrown from crates/app-jni via JNIEnv::throw_new("sh/amux/core/runtime/BridgeException", ...).
-keep class sh.amux.core.runtime.BridgeException {
    public <init>(java.lang.String);
}

# kotlinx.serialization ships its own R8 rules for the serializers its plugin
# generates, and the default optimize rules keep enums' values() and valueOf(),
# so the values crossing the bridge need nothing here. The release launch smoke
# test (`just android release-smoke`) decodes them on the minified build.
