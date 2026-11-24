package sh.amux.build;

import com.android.tools.r8.ClassFileConsumer;
import com.android.tools.r8.CompilationMode;
import com.android.tools.r8.R8;
import com.android.tools.r8.R8Command;
import com.android.tools.r8.origin.Origin;
import java.io.IOException;
import java.io.OutputStream;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.Enumeration;
import java.util.List;
import java.util.zip.ZipEntry;
import java.util.zip.ZipFile;
import java.util.zip.ZipOutputStream;

/**
 * R8 behind ProGuard's command line, which is what Buck2's Android rules run
 * to shrink a release build (`java -jar proguard.jar @args`, with an
 * `-injars`/`-outjars` pair per input jar).
 *
 * <p>R8 shrinks the whole program into one output, so every class lands in
 * the first output jar and the others are left empty: the rules dex what
 * they get, and an empty jar dexes to nothing. The output is class files,
 * which the rules then dex as they would ProGuard's. Everything else in the
 * arguments is ProGuard configuration, which R8 reads as it is (Gradle hands
 * R8 the same `proguard-rules.pro`), apart from the options that name
 * outputs or a base directory, which R8 has no use for here.
 *
 * <p>Libraries carry their own rules inside their jars (kotlinx.serialization
 * keeps its serializers that way), which Gradle's Android plugin reads and
 * R8 alone doesn't; this reads them the same way.
 */
public final class R8Proguard {
  private R8Proguard() {}

  public static void main(String[] args) throws Exception {
    List<String> tokens = new ArrayList<>();
    for (String arg : args) {
      if (arg.startsWith("@")) {
        tokens.addAll(tokenize(Files.readString(Paths.get(arg.substring(1)))));
      } else {
        tokens.add(arg);
      }
    }
    List<Path> outputs = new ArrayList<>();
    StringBuilder config = new StringBuilder();
    for (int i = 0; i < tokens.size(); i++) {
      String token = tokens.get(i);
      switch (token) {
        case "-outjars" -> outputs.add(Paths.get(unquote(tokens.get(++i))));
        case "-injars" -> {
          String jar = tokens.get(++i);
          config.append("\n-injars ").append(jar);
          for (String rules : embeddedRules(Paths.get(unquote(jar)))) {
            config.append('\n').append(rules);
          }
        }
        // `<user.dir>`, the directory it runs in, which relative paths
        // already mean.
        case "-basedirectory" -> i++;
        default -> {
          if (token.startsWith("-")) {
            config.append('\n');
          } else {
            config.append(' ');
          }
          config.append(token);
        }
      }
    }
    if (outputs.isEmpty()) {
      throw new IllegalArgumentException("no -outjars given");
    }
    // Lambdas and string concatenation compile to calls on these JDK
    // classes, which android.jar doesn't have: dexing turns the calls into
    // ordinary code, so R8 writing class files leaves them for D8.
    config.append("\n-dontwarn java.lang.invoke.LambdaMetafactory");
    config.append("\n-dontwarn java.lang.invoke.StringConcatFactory");
    for (Path output : outputs.subList(1, outputs.size())) {
      writeEmptyJar(output);
    }
    R8.run(
        R8Command.builder()
            .addProguardConfiguration(List.of(config.toString()), Origin.unknown())
            .setProgramConsumer(new ClassFileConsumer.ArchiveConsumer(outputs.get(0), true))
            .setMode(CompilationMode.RELEASE)
            .build());
  }

  /**
   * The rules a jar carries for R8: those under
   * META-INF/com.android.tools/ for this version of R8 (`r8`,
   * `r8-from-1.6.0`, `r8-upto-3.0.0`, `r8-from-1.5.0-upto-3.0.0`), or, when
   * it has none of those, its META-INF/proguard/ ones, as the Android
   * Gradle plugin chooses.
   */
  static List<String> embeddedRules(Path jar) throws IOException {
    List<String> targeted = new ArrayList<>();
    List<String> legacy = new ArrayList<>();
    boolean hasTargeted = false;
    int[] version = parseVersion(com.android.tools.r8.Version.getVersionString().split(" ")[0]);
    try (ZipFile zip = new ZipFile(jar.toFile())) {
      Enumeration<? extends ZipEntry> entries = zip.entries();
      while (entries.hasMoreElements()) {
        ZipEntry entry = entries.nextElement();
        String name = entry.getName();
        if (entry.isDirectory()) {
          continue;
        }
        if (name.startsWith("META-INF/com.android.tools/")) {
          hasTargeted = true;
          String[] parts = name.substring("META-INF/com.android.tools/".length()).split("/", 2);
          if (parts.length == 2 && appliesTo(parts[0], version)) {
            targeted.add(read(zip, entry));
          }
        } else if (name.startsWith("META-INF/proguard/")) {
          legacy.add(read(zip, entry));
        }
      }
    }
    return hasTargeted ? targeted : legacy;
  }

  static boolean appliesTo(String directory, int[] version) {
    if (directory.equals("r8")) {
      return true;
    }
    if (!directory.startsWith("r8-")) {
      return false;
    }
    String rest = directory.substring(3);
    String from = null;
    String upto = null;
    if (rest.startsWith("from-")) {
      rest = rest.substring(5);
      int split = rest.indexOf("-upto-");
      if (split >= 0) {
        from = rest.substring(0, split);
        upto = rest.substring(split + 6);
      } else {
        from = rest;
      }
    } else if (rest.startsWith("upto-")) {
      upto = rest.substring(5);
    } else {
      return false;
    }
    return (from == null || compare(version, parseVersion(from)) >= 0)
        && (upto == null || compare(version, parseVersion(upto)) < 0);
  }

  private static int[] parseVersion(String text) {
    String[] parts = text.split("-")[0].split("\\.");
    int[] version = new int[3];
    for (int i = 0; i < Math.min(3, parts.length); i++) {
      version[i] = Integer.parseInt(parts[i]);
    }
    return version;
  }

  private static int compare(int[] a, int[] b) {
    for (int i = 0; i < 3; i++) {
      if (a[i] != b[i]) {
        return Integer.compare(a[i], b[i]);
      }
    }
    return 0;
  }

  private static String read(ZipFile zip, ZipEntry entry) throws IOException {
    return new String(zip.getInputStream(entry).readAllBytes(), StandardCharsets.UTF_8);
  }

  /** Whitespace-separated words, a double-quoted run being one word. */
  private static List<String> tokenize(String text) {
    List<String> words = new ArrayList<>();
    StringBuilder word = new StringBuilder();
    boolean quoted = false;
    boolean any = false;
    for (char c : text.toCharArray()) {
      if (c == '"') {
        quoted = !quoted;
        word.append(c);
        any = true;
      } else if (Character.isWhitespace(c) && !quoted) {
        if (any) {
          words.add(word.toString());
          word.setLength(0);
          any = false;
        }
      } else {
        word.append(c);
        any = true;
      }
    }
    if (any) {
      words.add(word.toString());
    }
    return words;
  }

  private static String unquote(String word) {
    return word.length() >= 2 && word.startsWith("\"") && word.endsWith("\"")
        ? word.substring(1, word.length() - 1)
        : word;
  }

  private static void writeEmptyJar(Path path) throws IOException {
    Files.createDirectories(path.toAbsolutePath().getParent());
    try (OutputStream out = Files.newOutputStream(path);
        ZipOutputStream zip = new ZipOutputStream(out)) {
      zip.finish();
    }
  }
}
