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

# Guard kotlinx.serialization generated serializers and sealed/enum descriptors under R8 full mode.
-keepattributes *Annotation*, InnerClasses
-dontnote kotlinx.serialization.AnnotationsKt

-keepclassmembers class kotlinx.serialization.json.** {
    *** Companion;
}
-keepclasseswithmembers class kotlinx.serialization.json.** {
    kotlinx.serialization.KSerializer serializer(...);
}
-keep,includedescriptorclasses class sh.amux.**$$serializer { *; }
-keepclassmembers class sh.amux.** {
    *** Companion;
}
-keepclasseswithmembers class sh.amux.** {
    kotlinx.serialization.KSerializer serializer(...);
}
-keep enum sh.amux.** {
    public static **[] values();
    public static ** valueOf(java.lang.String);
}
