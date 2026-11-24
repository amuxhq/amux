# Added to the release build only for its launch smoke test
# (`just android release-smoke`).
#
# The test APK leaves out every library the app already links, so what the
# test and its runner call in those libraries must survive R8 in the app, even
# where the app itself never calls it.
-keep class androidx.tracing.** { *; }
-keep class kotlin.** { *; }
-keep class kotlinx.coroutines.BuildersKt { *; }

# The app's own classes and calls the test names, which R8 would otherwise
# rename, or inline into their one caller: the test APK is compiled against
# their source names and isn't rewritten to R8's. Only the names and calls
# are kept: the serializers and the JNI surface they reach stay exactly as
# proguard-rules.pro leaves them, which is what the test proves.
-keep class sh.amux.core.runtime.Runtime {
    public *;
}
-keep class sh.amux.core.runtime.Runtime$Companion {
    public *;
}
-keep class sh.amux.values.StartConfig {
    public <init>(...);
}
-keep class sh.amux.values.ProfileView {
    public java.lang.String getId();
}
