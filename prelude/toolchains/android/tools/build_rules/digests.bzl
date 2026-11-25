# Written by amux's scripts/prelude.py, which says why: each Android
# toolchain download's SHA-256 and size, by the SHA-1 its BUCK file gives.
# None is a download its origin doesn't serve (it 404s), which nothing
# amux builds uses: it keeps its SHA-1 alone, as upstream declares it.
DIGESTS = {
    # https://repo1.maven.org/maven2/org/ow2/asm/asm/9.7/asm-9.7.jar
    "073d7b3086e14beb604ced229c302feff6449723": ("adf46d5e34940bdf148ecdd26a9ee8eea94496a72034ff7141066b3eea5c4e9d", 125428),
    # https://repo1.maven.org/maven2/io/grpc/grpc-netty/1.24.0/grpc-netty-1.24.0.jar
    "07a9669aa35f447d5181edc1b4f4e887128c045b": ("8478333706ba442a354c2ddb8832d80a5aef71016e8a9cf07e7bf6e8c298f042", 234811),
    # https://maven.google.com/com/android/tools/sdklib/31.7.3/sdklib-31.7.3.jar
    "0adb7cac2ab282100e2b8b6d66b997a69ed06adf": ("0a3f7097a4a00b384046b783b7053a59ec0bd92256ab84baa6bd807aa02858b3", 1724128),
    # https://repo1.maven.org/maven2/com/android/tools/build/manifest-merger/31.7.3/manifest-merger-31.7.3.jar
    "0d0489e76e4eb785336317add6b65d57b928a429": None,
    # https://repo1.maven.org/maven2/net/bytebuddy/byte-buddy-agent/1.15.10/byte-buddy-agent-1.15.10.jar
    "0e8eb255b2c378b9a6c7341e7b0e12f0a5636377": ("dedd53e846898a4c494cb917de1a2816e2f39e05d86568501576343f2d924ba1", 365425),
    # https://repo1.maven.org/maven2/org/objenesis/objenesis/3.3/objenesis-3.3.jar
    "1049c09f1de4331e8193e579448d0916d75b7631": ("02dfd0b0439a5591e35b708ed2f5474eb0948f53abf74637e959b8e4ef69bfeb", 49423),
    # https://repo1.maven.org/maven2/net/java/dev/jna/jna/5.13.0/jna-5.13.0.jar
    "1200e7ebeedbe0d10062093f32925a912020e747": ("66d4f819a062a51a1d5627bffc23fac55d1677f0e0a1feba144aabdd670a64bb", 1879325),
    # https://repo1.maven.org/maven2/io/netty/netty-transport-native-kqueue/4.1.100.Final/netty-transport-native-kqueue-4.1.100.Final-osx-x86_64.jar
    "135c3a560a4fb84b0e2084f376610347483e2e8a": ("9ca2ba0121e35ea25228cf27ec553279966ca0a7f9c61e33ad1d9ea50477360d", 25020),
    # https://repo1.maven.org/maven2/com/google/devtools/ksp/symbol-processing-api/2.3.9/symbol-processing-api-2.3.9.jar
    "17799fb29499ca8f2a6e8e26d5d4b47c8af9d617": ("53e4bb74b7e65d065222739735f9f2f57eedbaebbd314fa69167bd43dadcb659", 105488),
    # https://repo1.maven.org/maven2/org/hamcrest/hamcrest/2.2/hamcrest-2.2.jar
    "1820c0968dba3a11a1b30669bb1f01978a91dedc": ("5e62846a89f05cd78cd9c1a553f340d002458380c320455dd1f8fc5497a8a1c1", 123360),
    # https://repo1.maven.org/maven2/io/netty/netty-tcnative-boringssl-static/2.0.25.Final/netty-tcnative-boringssl-static-2.0.25.Final.jar
    "185980556f9f083b5339825f19c9641c6c879417": ("96d9c14ab4c47cbad7fec9bdb083917db971d3754d6c7fa89f958bc719e230ed", 3108312),
    # https://github.com/facebook/buck2/releases/download/androidToolchain%2F2025-04-03/jar_builder_main.jar
    "18f839ebb58babae4ae35900ef33cdf065d0cbc5": ("f3f5ee7ec6639a2a6524bbd8485515a80b8aacbc76f76412d7d3304bf905fe9a", 22113272),
    # https://maven.google.com/com/android/tools/annotations/31.7.3/annotations-31.7.3.jar
    "1946e7f75bc1615b4abfc0f83899934dfcbdf1fc": ("b25995fa7b220d35fbb8e325df071f8291691bfbaff9734c98e38f45ec2a73ee", 11068),
    # https://github.com/facebook/buck2/releases/download/androidToolchain%2F2025-04-03/cp_snapshot_generator.jar
    "1bbf0a79f5c88a4bb316ebd98dfca7b615aae52a": ("a431403048a909e02d9d3f4cfba32a607a3e3feff288c70f5829ada2b4580c64", 60666465),
    # https://repo1.maven.org/maven2/org/tukaani/xz/1.9/xz-1.9.jar
    "1ea4bec1a921180164852c65006d928617bd2caf": ("211b306cfc44f8f96df3a0a3ddaf75ba8c5289eed77d60d72f889bb855f535e5", 116013),
    # https://repo1.maven.org/maven2/com/fasterxml/jackson/datatype/jackson-datatype-jdk8/2.18.0/jackson-datatype-jdk8-2.18.0.jar
    "2046c6ad99a59b5bccf840021f5b3dcf2890acc2": ("01808b19dfd7bfb0334ccc508c02292ce87bc518a8fcdd7e1b62ba8be589aa00", 35531),
    # https://repo1.maven.org/maven2/com/google/devtools/ksp/symbol-processing-aa-embeddable/2.3.9/symbol-processing-aa-embeddable-2.3.9.jar
    "214defea18ed4a0030e3d129c1831e266072b7ec": ("b3536a0e599eaf1c150a42ab089d8d82aeec35fb2546c937f4d37f4c5ecb7998", 84501468),
    # https://repo1.maven.org/maven2/io/netty/netty-transport-native-epoll/4.1.100.Final/netty-transport-native-epoll-4.1.100.Final-linux-aarch_64.jar
    "22c3fe045620e6e05002ed1999d276dc4176a7a4": ("81993265c3fb9098c0163943e71c14fbaa6ed17e345e7bf8205c90acfdf6cef5", 40892),
    # https://github.com/facebook/buck2/releases/download/androidToolchain%2F2025-04-03/zip_scrubber_main.jar
    "23c85d8db2958ad502c1cd1eb3bbcd1342c37c7d": ("ae71f570ac36d9af7fa5d2e38dde8165ca5ace2637cebd1147388f3a11ebdec2", 10388458),
    # https://repo1.maven.org/maven2/args4j/args4j/2.37/args4j-2.37.jar
    "244f60c057d72a785227c0562d3560f42a7ea54b": ("5a13b0f7ff519183e55b8f453b8faaa23b92ccf0443f1fac515fedf2246bdc18", 158359),
    # https://repo1.maven.org/maven2/com/facebook/infer/annotation/infer-annotation/0.18.0/infer-annotation-0.18.0.jar
    "27539793fe93ed7d92b6376281c16cda8278ab2f": ("caefba935643dc962758a760f1627a402215af034d96c0f484d63b7fddca992f", 20583),
    # https://repo1.maven.org/maven2/org/opentest4j/opentest4j/1.2.0/opentest4j-1.2.0.jar
    "28c11eb91f9b6d8e200631d46e20a7f407f2a046": ("58812de60898d976fb81ef3b62da05c6604c18fd4a249f5044282479fc286af2", 7653),
    # https://repo1.maven.org/maven2/io/grpc/grpc-api/1.24.0/grpc-api-1.24.0.jar
    "2b21b84ab14f632b01281ef2d616a3ed96588f17": ("553978366e04ee8ddba64afde3b3cf2ac021a2f3c2db2831b6491d742b558598", 217502),
    # https://repo1.maven.org/maven2/com/google/devtools/ksp/symbol-processing-common-deps/2.3.9/symbol-processing-common-deps-2.3.9.jar
    "2c1ecd8fae549f14c7c84244729383d47ab85c51": ("ef51e88dad8a839ac0ddea0e66d19ac14c5603f3500995db8fcab1f1be5fd3b9", 119558),
    # https://repo1.maven.org/maven2/io/netty/netty-transport-native-kqueue/4.1.100.Final/netty-transport-native-kqueue-4.1.100.Final-osx-aarch_64.jar
    "2d75a8e80a75def2b61bac1ea5f66d184f5069f0": ("0fdc323ad2efd85343713fbce6e0c2aec27e6852659984d03a5414018e01815f", 25582),
    # https://repo1.maven.org/maven2/com/squareup/kotlinpoet-jvm/1.16.0/kotlinpoet-jvm-1.16.0.jar
    "2df03e8b2afdb98dda7b6b0753cd5d9b12b40031": ("41a506824da430c0e543a023243eae88dba1888acf255b999a51da2dbefc0bc8", 351371),
    # https://repo1.maven.org/maven2/org/junit/platform/junit-platform-commons/1.8.2/junit-platform-commons-1.8.2.jar
    "32c8b8617c1342376fd5af2053da6410d8866861": ("d2e015fca7130e79af2f4608dc54415e4b10b592d77333decb4b1a274c185050", 100450),
    # https://repo1.maven.org/maven2/com/google/protobuf/protobuf-java-util/3.25.5/protobuf-java-util-3.25.5.jar
    "38cc5ce479603e36466feda2a9f1dfdb2210ef00": ("dacc58b2c3d2fa8d4bddc1acb881e78d6cf7c137dd78bc1d67f6aca732436a8d", 73140),
    # https://repo1.maven.org/maven2/io/netty/netty-buffer/4.1.100.Final/netty-buffer-4.1.100.Final.jar
    "39b05d2d4027971bf99111a9be1d7035a116bb55": ("462874b44ee782fbefec64078cda6eb8e7bf9f0e0af71a928ef4c1f2d564f7ee", 306739),
    # https://repo1.maven.org/maven2/org/jetbrains/intellij/deps/trove4j/1.0.20200330/trove4j-1.0.20200330.jar
    "3afb14d5f9ceb459d724e907a21145e8ff394f02": ("c5fd725bffab51846bf3c77db1383c60aaaebfe1b7fe2f00d23fe1b7df0a439d", 572985),
    # https://repo1.maven.org/maven2/io/grpc/protoc-gen-grpc-java/1.10.1/protoc-gen-grpc-java-1.10.1-linux-x86_64.exe
    "3c8950cae9d5b4bed363f71118fb54bfddafcd2a": ("7b96e0ebeafda3640cc79d96cd7e60dae372a1c744e9aacc9ee726c2db7a5026", 2643480),
    # https://github.com/facebook/buck2/releases/download/androidToolchain%2F2025-04-03/d8_dexer_patched.jar
    "3d41953fbdda2adf1a75baee10851cd9de7c1b24": ("f85ec538b9c1e314a0d16dbc45bf3754ead5b5827107ddbe7e90f43f83aba2af", 49956889),
    # https://repo1.maven.org/maven2/org/jspecify/jspecify/1.0.1/jspecify-1.0.1.jar
    "3d60fd98eb8ade73004f4195c37b6317e02cf3d7": ("070d75f261fe4c5b8202508366715f7f2d4660f88c8ef7e6d3575e48c9683b66", 3064),
    # https://repo1.maven.org/maven2/org/jetbrains/kotlin/kotlin-metadata-jvm/2.3.21/kotlin-metadata-jvm-2.3.21.jar
    "44a462b3cacd654503a791e4a04ec84f7a1f1c78": ("00590f26a5a768777f04252c6cf93c016a3361a6bc93636df67375ad4d19557d", 902765),
    # https://repo1.maven.org/maven2/io/netty/netty-handler/4.1.100.Final/netty-handler-4.1.100.Final.jar
    "4c0acdb8bb73647ebb3847ac2d503d53d72c02b4": ("0e10e584c2e7fdf7f4804e14760ed987003f1b62ab982f62eaf13a9892793d3a", 561288),
    # https://repo1.maven.org/maven2/org/junit/jupiter/junit-jupiter-api/5.8.2/junit-jupiter-api-5.8.2.jar
    "4c21029217adf07e4c0d0c5e192b6bf610c94bdc": ("1808ee87e0f718cd6e25f3b75afc17956ac8a3edc48c7e9bab9f19f9a79e3801", 193500),
    # https://repo1.maven.org/maven2/org/jetbrains/kotlin/kotlin-annotation-processing-embeddable/2.3.21/kotlin-annotation-processing-embeddable-2.3.21.jar
    "4d68676c8b1a36043d3188f0de16f6842709bc1f": ("7e8e36c0a349d67244c41e43adb4c482fa18ce0db042306515540f6c9ac2d476", 479140),
    # https://maven.google.com/com/android/tools/ddms/ddmlib/31.7.3/ddmlib-31.7.3.jar
    "525b44de6936a7c487095652f014d0bfc84bb9ad": ("0d7ef53823f6a21b807f398f0d039a5dde184b58c5589625626ca4aa3d6108ed", 671730),
    # https://github.com/facebook/buck2/releases/download/androidToolchain%2F2025-04-03/api-stubber-main_jar.jar
    "59b1fcca9ec544a86df64fb544fce3bfcee7eaa8": ("986394f907cadf76b6992471df91cbd1c34018cd87a5fd1d031320133a9f8850", 28974683),
    # https://repo1.maven.org/maven2/com/google/protobuf/protobuf-java/3.25.5/protobuf-java-3.25.5.jar
    "5ae5c9ec39930ae9b5a61b32b93288818ec05ec1": ("8540247fad9e06baefa8fb45eb313802d019f485f14300e0f9d6b556ed88e753", 1875414),
    # https://maven.google.com/com/android/tools/build/aapt2-proto/8.7.3-12006047/aapt2-proto-8.7.3-12006047.jar
    "605bc2d642e5812469ff0333df1b65ead289ccd8": ("b32f7d4b2975ac9ee7f06054c18f1fea9da045dc1a61ff436ecf543480036302", 711278),
    # https://repo1.maven.org/maven2/com/googlecode/plist/dd-plist/1.28/dd-plist-1.28.jar
    "617d08cc6e9c800e4b77266087cc9294b6d87e11": ("88ed8e730f7386297485176c4387146c6914a38c0e58fc296e8a01cdc3b621e1", 100576),
    # https://repo1.maven.org/maven2/net/bytebuddy/byte-buddy/1.15.10/byte-buddy-1.15.10.jar
    "635c873fadd853c084f84fdc3cbd58c5dd8537f9": ("d8390d20685a41a2bdca640f958942cd91bcbf21c42470494bdf5752d9a07b14", 8470015),
    # https://repo1.maven.org/maven2/io/netty/netty-transport-native-unix-common/4.1.100.Final/netty-transport-native-unix-common-4.1.100.Final.jar
    "648ff5571022dbfa6789122e3872477bbf67fa7b": ("5d888230a04c4a3e647c64e933cefb64fd49056f969bfb734c8a3fcedf0bea8a", 43968),
    # https://repo1.maven.org/maven2/org/apache/commons/commons-compress/1.26.0/commons-compress-1.26.0.jar
    "659feffdd12280201c8aacb8f7be94f9a883c824": ("051aceb8bbcc62d0f5b2b8ac72c53767f9c59bfbd050151e65bef6f51c8ed9c9", 1078328),
    # https://repo1.maven.org/maven2/com/fasterxml/jackson/core/jackson-core/2.18.0/jackson-core-2.18.0.jar
    "65e8ead7de5d8f7a53e296c363bea3182f21f925": ("215bbd7c8fd65be504cb92ff3aa1c4b790fc7b14cca72f4546aac4143c101bb5", 596823),
    # https://repo1.maven.org/maven2/io/netty/netty-transport/4.1.100.Final/netty-transport-4.1.100.Final.jar
    "6620fbfb47667a5eb6050e35c7b4c88000bcd77f": ("b1deeceedab3734cdb959c55f4be5ab4a667a8aed59121ff93763f49470f5470", 489999),
    # https://repo1.maven.org/maven2/org/testng/testng/7.5.1/testng-7.5.1.jar
    "6673f647a35826fb01ff3129edc241d88bd27b88": ("a5ac92d2362ccb3a509abe68e385ca809a7c96fcbaf851b3ee8bacb2ac899e2f", 943465),
    # https://repo1.maven.org/maven2/javax/inject/javax.inject/1/javax.inject-1.jar
    "6975da39a7040257bd51d21a231b76c915872d38": ("91c77044a50c481636c32d916fd89c9118a72195390452c81065080f957de7ff", 2497),
    # https://maven.google.com/com/android/tools/layoutlib/layoutlib-api/31.7.3/layoutlib-api-31.7.3.jar
    "6abada4555fa60825ea5f353443a7e561445c4f0": ("98bae36ff051e114dd4eef5effa80fbe5e552944f4233ce892160da2cd9eeaed", 121732),
    # https://repo1.maven.org/maven2/org/jetbrains/kotlin/kotlin-reflect/2.3.21/kotlin-reflect-2.3.21.jar
    "6acc55c52691ce3fd41b20a0472f3513106521a5": ("33e37d9dfa86c7a29ab34e975b5f4a1287b458878611051f3485652220c1a2cb", 3633922),
    # https://repo1.maven.org/maven2/org/mockito/kotlin/mockito-kotlin/6.1.0/mockito-kotlin-6.1.0.jar
    "6c89533c7c38b1600ceea37b806cba3d9619c1ac": ("efd8f8febafdccb63ee44fc9c7bca22fa642a03b1153f8c7a3391ddc64cdd6ac", 74796),
    # https://repo1.maven.org/maven2/com/fasterxml/jackson/datatype/jackson-datatype-guava/2.18.0/jackson-datatype-guava-2.18.0.jar
    "731e354fcae2bc14cb6a4deeef9c7e5ea620a11f": ("0fe2dcd853f23cc1056ac25a812b4a078726b2c15e22157120d6c282b855c896", 104395),
    # https://repo1.maven.org/maven2/com/google/j2objc/j2objc-annotations/3.0.0/j2objc-annotations-3.0.0.jar
    "7399e65dd7e9ff3404f4535b2f017093bdb134c7": ("88241573467ddca44ffd4d74aa04c2bbfd11bf7c17e0c342c94c9de7a70a7c64", 12467),
    # https://repo1.maven.org/maven2/com/google/dagger/dagger/2.51/dagger-2.51.jar
    "75786d67abe43cac5b61f39a6101276184e708f8": ("a68b07ce740353462b04c9ad5944d9c22831025a4c8fbf282e3708a5684b68f7", 45524),
    # https://repo1.maven.org/maven2/io/netty/netty-transport-classes-epoll/4.1.100.Final/netty-transport-classes-epoll-4.1.100.Final.jar
    "78489936ca1d91483e34a31d04a3b0812386eb39": ("9abc4b17b1212b33666eae4e8013d0bb78a9a2bcd0a9a621b9bd06a7e5fc0050", 147139),
    # https://repo1.maven.org/maven2/io/grpc/grpc-stub/1.24.0/grpc-stub-1.24.0.jar
    "7bd32e6bed0389b4d547ac617840c5ebd54768fd": ("eaa9201896a77a0822e26621b538c7154f00441a51c9b14dc9e1ec1f2acfb815", 40129),
    # https://repo1.maven.org/maven2/com/googlecode/java-diff-utils/diffutils/1.3.0/diffutils-1.3.0.jar
    "7e060dd5b19431e6d198e91ff670644372f60fbd": ("61ba4dc49adca95243beaa0569adc2a23aedb5292ae78aa01186fa782ebdc5c2", 34130),
    # https://repo1.maven.org/maven2/io/netty/netty-common/4.1.100.Final/netty-common-4.1.100.Final.jar
    "847f942381145de23f21c836d05b0677474271d3": ("d2908301f1ac6f2910900742473c15d701765d3d4467acdb1eebb9df3aa82885", 660474),
    # https://maven.google.com/com/android/tools/build/bundletool/1.16.0/bundletool-1.16.0.jar
    "88e6c67d315e7d2a860bb00b1bd93914f2c4fd33": ("1ea2bf5274bbac7a3bb5618521d2fa11fb04e900e33a8a646029ec6332fc08c8", 13260076),
    # https://repo1.maven.org/maven2/junit/junit/4.13.2/junit-4.13.2.jar
    "8ac9e16d933b6fb43bc7f576336b8f4d7eb5ba12": ("8e495b634469d64fb8acfa3495a065cbacc8a0fff55ce1e31007be4c16dc57d3", 384581),
    # https://repo1.maven.org/maven2/org/jetbrains/kotlin/kotlin-daemon-client/2.3.21/kotlin-daemon-client-2.3.21.jar
    "8bd63e265ea5e6472f96f7e9e2431d02b4e69f3e": ("fa767c9b40d069f164c617a6e6bd302643c7ba8c0792e8ed9e412c18f84b50be", 243178),
    # https://repo1.maven.org/maven2/io/grpc/protoc-gen-grpc-java/1.10.1/protoc-gen-grpc-java-1.10.1-osx-x86_64.exe
    "8c37e3794df02dcb2dcc7547266119bc09d3aeae": ("14d10f6c9d81511c6e29eab6b7f61ef9c9818a402d085030006c3308c2e0ec0d", 4771344),
    # https://repo1.maven.org/maven2/org/jetbrains/kotlin/kotlin-script-runtime/2.3.21/kotlin-script-runtime-2.3.21.jar
    "8dafd7f0eb5efef35a446683a2774fef61c68aeb": ("ec61bf1229c837fa9f4255f34ca69816138b73158e7ae9b8d964cf92ca6dfd2e", 44972),
    # https://repo1.maven.org/maven2/com/fasterxml/jackson/core/jackson-databind/2.18.0/jackson-databind-2.18.0.jar
    "8dba1f789a75fc30b59303574fe2b269afa4d3bc": ("2bf1927b7f3224683ed0157a1ec3b0ede75179da3e597d78c572d56ed00f9f3c", 1663219),
    # https://repo1.maven.org/maven2/com/google/protobuf/protoc/3.25.6/protoc-3.25.6-linux-x86_64.exe
    "9065c462198f65a0f5ab4945023dac99288cf24f": ("a13450e63255d9e0d311feb6de70de0356b50f7791dc01cb9ce3732b74a41c69", 8852624),
    # https://repo1.maven.org/maven2/org/jetbrains/annotations/13.0/annotations-13.0.jar
    "919f0dfe192fb4e063e7dacadee7f8bb9a2672a9": ("ace2a10dc8e2d5fd34925ecac03e4988b2c0f851650c94b8cef49ba1bd111478", 17536),
    # https://repo1.maven.org/maven2/com/google/jimfs/jimfs/1.3.0/jimfs-1.3.0.jar
    "93096472c3654a761c40c8e5d4ad82a7c1a0fd54": ("82494408bb513f5512652e7b7f63d6f31f01eff57ce35c878644ffc2d25aee4f", 211713),
    # https://repo1.maven.org/maven2/io/netty/netty-codec-http/4.1.100.Final/netty-codec-http-4.1.100.Final.jar
    "992623e7d8f2d96e41faf1687bb963f5433e3517": ("326811d249cb0e5555e78e026e877834e792261c38f0666d80464426695d9590", 657672),
    # https://repo1.maven.org/maven2/org/jetbrains/kotlin/kotlin-build-tools-api/2.3.21/kotlin-build-tools-api-2.3.21.jar
    "9b0a49a4d4c570c63a3a2553816bf0539536ab69": ("b2e5cc2666c100079ed343618528eb820dcfa5095095e30f2dd19c60dff7e462", 168792),
    # https://repo1.maven.org/maven2/com/fasterxml/jackson/core/jackson-annotations/2.18.0/jackson-annotations-2.18.0.jar
    "9bddcc56af9d90f902ef5dba7348102cd12b04e2": ("a09367d2eeb526873abf737ff754c5085f82073a382ee72c3bbe15412217671f", 79108),
    # https://repo1.maven.org/maven2/org/jetbrains/kotlinx/kotlinx-coroutines-core-jvm/1.9.0/kotlinx-coroutines-core-jvm-1.9.0.jar
    "9beade4c1c1569e4f36cbd2c37e02e3e41502601": ("ad89c2892235e670f222d819cb3d81188143cb19a05b59df9889ae4269f5c70a", 1463587),
    # https://repo1.maven.org/maven2/io/netty/netty-codec/4.1.100.Final/netty-codec-4.1.100.Final.jar
    "9c3c71e7cf3b8ce3bfc9fa52a524b9ca7ddf259c": ("180a01ed67af399602e24ff1c32864e7f57f57c4a0fa5e9ab3fe9b0e5e9cf051", 345293),
    # https://repo1.maven.org/maven2/org/jetbrains/kotlin/jvm-abi-gen/2.3.21/jvm-abi-gen-2.3.21.jar
    "9d6dad64b4468307ac8ddbe4d4712a208c7b1738": ("88a5e104dde4ef2f3cd211b077bb93f0a5e2e54b61e53c373f146af061be4504", 969316),
    # https://repo1.maven.org/maven2/org/apiguardian/apiguardian-api/1.1.2/apiguardian-api-1.1.2.jar
    "a231e0d844d2721b0fa1b238006d15c6ded6842a": ("b509448ac506d607319f182537f0b35d71007582ec741832a1f111e5b5b70b38", 6806),
    # https://repo1.maven.org/maven2/org/jetbrains/kotlin/kotlin-stdlib/2.3.21/kotlin-stdlib-2.3.21.jar
    "a2ee2c4e220fee522f0451b723bab2a43f3481a7": ("6f64eac736db9434dd6925b4a518b9d1d17177652320c37916cf9ba3ce7d7d7a", 1804721),
    # https://repo1.maven.org/maven2/io/netty/netty-transport-classes-kqueue/4.1.100.Final/netty-transport-classes-kqueue-4.1.100.Final.jar
    "a32c1b13b82bfd7f09d4ea4836257d48056f0fe2": ("70b40962b01ab594bed5cd311cd8768b224155c948118af60412ab23bc55ed50", 108428),
    # https://repo1.maven.org/maven2/org/mockito/mockito-core/5.20.0/mockito-core-5.20.0.jar
    "a32f446f38acf636363c5693db6498047731b9e0": ("d1a96d252128d3a4247cfd8a2e76412efa3cc103977be17933c942117a24f374", 710508),
    # https://repo1.maven.org/maven2/com/google/protobuf/protoc/3.25.6/protoc-3.25.6-osx-x86_64.exe
    "a57fab7d37d8824034cc67325f81de24d0f36b6a": ("d55ac3681a454e4827af768f311a97c7583b09fef483e25c8e487a2a9c263bb7", 7250656),
    # https://repo1.maven.org/maven2/jakarta/annotation/jakarta.annotation-api/1.3.4/jakarta.annotation-api-1.3.4.jar
    "a858ec3f0ebd2b8d855c1ddded2cde9b381b0517": ("e65d0194ca63c73d11fcd0a73325e762d905aafb51c90cdb98a5a5968367564a", 25150),
    # https://repo1.maven.org/maven2/io/grpc/grpc-context/1.24.0/grpc-context-1.24.0.jar
    "abfd83c3b1a8a0fbc12db3529a339b693056f496": ("1f0546e18789f7445d1c5a157010a11bc038bbb31544cdb60d9da3848efcfeea", 30096),
    # https://repo1.maven.org/maven2/io/netty/netty-transport-native-kqueue/4.1.100.Final/netty-transport-native-kqueue-4.1.100.Final.jar
    "ae88188265e417b5c7b27cb880ecec2bd7dd8a35": ("67cebdff558142f52dd495e5c010ba448a900b2d91bb4d2fece900b0bc95ea3e", 5750),
    # https://repo1.maven.org/maven2/com/google/code/gson/gson/2.10.1/gson-2.10.1.jar
    "b3add478d4382b78ea20b1671390a858002feb6c": ("4241c14a7727c34feea6507ec801318a3d4a90f070e4525681079fb94ee4c593", 283367),
    # https://repo1.maven.org/maven2/com/github/luben/zstd-jni/1.5.7-17/zstd-jni-1.5.7-17.jar
    "b4fea046fcd925681fa10c61be99da059f4f745a": ("c9043e2da8a16c13b6cd7661a5cb56685a573c3546ef4bd0ead67b0b67c2e107", 6323239),
    # https://repo1.maven.org/maven2/io/grpc/protoc-gen-grpc-java/1.10.1/protoc-gen-grpc-java-1.10.1-windows-x86_64.exe
    "b5692a8df52f6aef24c7181f94240ef52b7060fb": ("28238c7b0e4c3143f9733b4631337ec06b0ff0d122c9b7911de3c3db401e0536", 2054144),
    # https://repo1.maven.org/maven2/io/grpc/grpc-protobuf/1.24.0/grpc-protobuf-1.24.0.jar
    "b584bf5e69fa8794a9184c1cf66d4c690f21cbc4": ("88cd0838ea32893d92cb214ea58908351854ed8de7730be07d5f7d19025dd0bc", 4994),
    # https://repo1.maven.org/maven2/org/junit/platform/junit-platform-engine/1.8.2/junit-platform-engine-1.8.2.jar
    "b737de09f19864bd136805c84df7999a142fec29": ("0b7d000f8c3e8e5f7d6b819649936e7b9938314e87c8f983805218ea57567e59", 185762),
    # https://repo1.maven.org/maven2/io/grpc/grpc-netty-shaded/1.24.0/grpc-netty-shaded-1.24.0.jar
    "b93f11824ee89d1283801dce8145bc54e1b8e94e": ("1e1145306cf5ef4f96d2cd2d234394530024574133d5318442d40c905831cdc5", 6943999),
    # https://repo1.maven.org/maven2/org/jetbrains/kotlinx/kotlinx-metadata-jvm/0.7.0/kotlinx-metadata-jvm-0.7.0.jar
    "babbf7cbcd7083709bf27ee5e19dd558d2dea39b": ("835a146fbb0f229c1b8968e88dca66e7ca567a98ad88d71219848e522ca894a7", 1168224),
    # https://repo1.maven.org/maven2/io/grpc/grpc-protobuf-lite/1.24.0/grpc-protobuf-lite-1.24.0.jar
    "be683c469b7652dc130d2304bdb32379816a4ccc": ("130326236fdb860ec466ed9c9e33c588bbcf058172493d584154eee0b5f13985", 7628),
    # https://repo1.maven.org/maven2/org/ow2/asm/asm-util/9.7/asm-util-9.7.jar
    "c0655519f24d92af2202cb681cd7c1569df6ead6": ("37a6414d36641973f1af104937c95d6d921b2ddb4d612c66c5a9f2b13fc14211", 93784),
    # https://repo1.maven.org/maven2/org/junit/platform/junit-platform-launcher/1.8.2/junit-platform-launcher-1.8.2.jar
    "c334fcee82b81311ab5c426ec2d52d467c8d0b28": ("822156409fd83e682e4c5199b3460054299b538a058c2c6d0f5c9b6a5bdb7594", 159468),
    # https://maven.google.com/com/android/tools/common/31.7.3/common-31.7.3.jar
    "c3df7b51ee1a72acf051e0e5efbeb8023fb4ea57": ("abb5b2f28954ac09dcfb5d192e355af41236c8337afc08b16aa1adc0e8aebf63", 569070),
    # https://repo1.maven.org/maven2/com/google/guava/failureaccess/1.0.2/failureaccess-1.0.2.jar
    "c4a06a64e650562f30b7bf9aaec1bfed43aca12b": ("8a8f81cf9b359e3f6dfa691a1e776985c061ef2f223c9b2c80753e1b458e8064", 4740),
    # https://repo1.maven.org/maven2/org/jetbrains/kotlin/kotlin-compiler-embeddable/2.3.21/kotlin-compiler-embeddable-2.3.21.jar
    "c4b47f8e33cdbeedd8c1415ab9f9ade870388162": ("d3e70fb011675e77ef00f1a68918d3cd91eed26058977d1e3ea2a37a293233af", 59720376),
    # https://repo1.maven.org/maven2/org/junit/jupiter/junit-jupiter-engine/5.8.2/junit-jupiter-engine-5.8.2.jar
    "c598b4328d2f397194d11df3b1648d68d7d990e3": ("753b7726cdd158bb34cedb94c161e2291896f47832a1e9eda53d970020a8184e", 229678),
    # https://repo1.maven.org/maven2/org/jetbrains/kotlin/kotlin-build-tools-impl/2.3.21/kotlin-build-tools-impl-2.3.21.jar
    "c6c11fd63ff15677c11f14dbdbad624f8d8dbf04": ("2cd70396404a0e43c05aaf79a8b35ff3a8ff2e296aedf770f6e5923fbfacae3b", 1461208),
    # https://repo1.maven.org/maven2/com/google/guava/listenablefuture/1.0/listenablefuture-1.0.jar
    "c949a840a6acbc5268d088e47b04177bf90b3cad": ("e4ad7607e5c0477c6f890ef26a49cb8d1bb4dffb650bab4502afee64644e3069", 3149),
    # https://repo1.maven.org/maven2/com/github/stefanbirkner/system-rules/1.16.1/system-rules-1.16.1.jar
    "cb9e42b003d0668f6445505c0f81dd9a244a05d5": ("3ec694ce9c07b624e2fb535abe03d675e50cf3f5f8185c9077a7af0e230d987d", 37535),
    # https://repo1.maven.org/maven2/io/netty/netty-codec-http2/4.1.100.Final/netty-codec-http2-4.1.100.Final.jar
    "cbf1a430ea44dbdedbcde16b185cbb95f28d72c7": ("23b4a74350f4cf8d41b93fb93d52b5050667d8d53fffc385672c86eab83b8749", 486355),
    # https://repo1.maven.org/maven2/net/sf/kxml/kxml2/2.3.0/kxml2-2.3.0.jar
    "ccbc77a5fd907ef863c29f3596c6f54ffa4e9442": ("f264dd9f79a1fde10ce5ecc53221eff24be4c9331c830b7d52f2f08a7b633de2", 43858),
    # https://repo1.maven.org/maven2/com/squareup/okhttp3/okhttp/4.10.0/okhttp-4.10.0.jar
    "cd63657ac15770ed1420647154c9f44645533bef": ("7580f14fa1691206e37081ad3f92063b1603b328da0bb316f2fef02e0562e7ec", 782603),
    # https://repo1.maven.org/maven2/io/netty/netty-transport-native-epoll/4.1.100.Final/netty-transport-native-epoll-4.1.100.Final.jar
    "cf3c3ef7cb7b30b58385d824dbaf85f5f72da221": ("34868795228df31914b75b10e97bf4d980e78e263569852e3185ca8c2a0de3e9", 5726),
    # https://maven.google.com/com/android/tools/sdk-common/31.7.3/sdk-common-31.7.3.jar
    "d17c3d28724a5f3e90a5ae053714627b740ff80c": ("4fa44a7d9675df13da614dee512df90ad3e4308d0addb32ead99f57ce58f675f", 1491331),
    # https://maven.google.com/com/android/tools/build/apksig/8.4.0/apksig-8.4.0.jar
    "d51ecbc4a5b7a2c7832586a4dde18f61d92810a4": ("c4f6fdf2148490296f422a7eb37647ea11d1c3b02f1f2349c33ce257f3c29b1f", 496313),
    # https://repo1.maven.org/maven2/io/netty/netty-transport-native-epoll/4.1.100.Final/netty-transport-native-epoll-4.1.100.Final-linux-x86_64.jar
    "d83003b8eac838e4bc3f7662a22f9f2d879c0fe4": ("a108fb47babb7678b5b2abb1fc8b34510bd2f705faa450447860647f2de4ebaa", 39373),
    # https://repo1.maven.org/maven2/com/google/protobuf/protoc/3.25.6/protoc-3.25.6-windows-x86_64.exe
    "d9bbbb5a88fc7fdcb41ff717028461f8e6b5b958": ("06f45952579be50e73e3f576c90413bfe94ff3602bff4cd290f4a1d1e04c364d", 11562431),
    # https://repo1.maven.org/maven2/com/google/inject/guice/5.1.0/guice-5.1.0.jar
    "da25056c694c54ba16e78e4fc35f17fc60f0d1b4": ("4130e50bfac48099c860f0d903b91860c81a249c90f38245f8fed58fc817bc26", 777563),
    # https://repo1.maven.org/maven2/com/google/guava/guava/33.2.0-jre/guava-33.2.0-jre.jar
    "e264781dadc4967e5292f3c4d05f1d153631f7b4": ("99f491e86262ce38d13b3581d40f77acdb4696a9505447c3154474c3192908dd", 3054490),
    # https://repo1.maven.org/maven2/org/ow2/asm/asm-tree/9.7/asm-tree-9.7.jar
    "e446a17b175bfb733b87c5c2560ccb4e57d69f1a": ("62f4b3bc436045c1acb5c3ba2d8ec556ec3369093d7f5d06c747eb04b56d52b1", 51934),
    # https://repo1.maven.org/maven2/io/opencensus/opencensus-contrib-grpc-metrics/0.24.0/opencensus-contrib-grpc-metrics-0.24.0.jar
    "ed76d8b99fb674478efa0c4c028ea9545ba1815f": ("875582e093f11950ad3f4a50b5fee33a008023f7d1e47820a1bef05d23b9ed42", 14965),
    # https://repo1.maven.org/maven2/com/fasterxml/jackson/module/jackson-module-kotlin/2.18.0/jackson-module-kotlin-2.18.0.jar
    "ef6ff9000dd9b28246e9346151127638d91398fa": ("f21118a355edba8d363cb395ded285d631345b74ee350d8c6e3cd84c5e182c2a", 185196),
    # https://repo1.maven.org/maven2/io/grpc/grpc-core/1.24.0/grpc-core-1.24.0.jar
    "efa08ef8d3d1de2a05fbb9b4c34ad6a053176206": ("8fc900625a9330b1c155b5423844d21be0a5574fe218a63170a16796c6f7880e", 612083),
    # https://repo1.maven.org/maven2/io/perfmark/perfmark-api/0.27.0/perfmark-api-0.27.0.jar
    "f86f575a41b091786a4b027cd9c0c1d2e3fc1c01": ("c7b478503ec524e55df19b424d46d27c8a68aeb801664fadd4f069b71f52d0f6", 6954),
    # https://repo1.maven.org/maven2/io/opencensus/opencensus-api/0.24.0/opencensus-api-0.24.0.jar
    "f974451b19007ce820f433311ce8adb88e2b7d2c": ("f561b1cc2673844288e596ddf5bb6596868a8472fd2cb8993953fc5c034b2352", 351668),
    # https://repo1.maven.org/maven2/com/squareup/okio/okio/1.17.6/okio-1.17.6.jar
    "fca77ce5f3ce6fa8a65738a31e34b87ccd289f71": ("8e88b055523cc80613dfb844d598740e10ada62f4bd98304cdd140c5854cc3b6", 92255),
}
