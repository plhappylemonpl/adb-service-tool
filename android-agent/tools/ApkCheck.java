import java.io.*;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.file.*;
import java.util.*;
import java.util.zip.*;

/**
 * Kontrola i naprawa układu archiwum APK (wymagania Androida 11+ / targetSdk 30+):
 *  - resources.arsc MUSI być zapisany bez kompresji (STORED),
 *  - dane każdego nieskompresowanego wpisu wyrównane do 4 B (biblioteki .so do 4096 B).
 *
 * Użycie:
 *   java -cp apkcheck.jar ApkCheck verify plik.apk            -> kod 0 = OK, 1 = błędy
 *   java -cp apkcheck.jar ApkCheck fix wejście.apk wyjście.apk -> resources.arsc STORED + wyrównanie
 *
 * Bez zależności (czysta Java 8+); zbudowane: javac --release 8 -d . ApkCheck.java && jar cf apkcheck.jar ApkCheck*.class
 */
public final class ApkCheck {

    private static final String ARSC = "resources.arsc";
    private static final int ALIGN = 4;
    private static final int SO_ALIGN = 4096;
    private static final int ALIGN_EXTRA_ID = 0xD935; // pole wyrównania jak w zipalign/apksig

    public static void main(String[] args) throws Exception {
        if (args.length == 2 && args[0].equals("verify")) {
            System.exit(verify(Paths.get(args[1])) ? 0 : 1);
        } else if (args.length == 3 && args[0].equals("fix")) {
            fix(Paths.get(args[1]), Paths.get(args[2]));
            System.exit(verify(Paths.get(args[2])) ? 0 : 1);
        } else {
            System.err.println("Użycie: ApkCheck verify <apk> | ApkCheck fix <wejście.apk> <wyjście.apk>");
            System.exit(2);
        }
    }

    // ------------------------------------------------------------------ odczyt centralnego katalogu

    static final class Entry {
        String name;
        int method;
        long localOffset;
        long dataOffset;
    }

    static List<Entry> entries(Path apk) throws IOException {
        byte[] all = Files.readAllBytes(apk);
        ByteBuffer b = ByteBuffer.wrap(all).order(ByteOrder.LITTLE_ENDIAN);
        int eocd = -1;
        for (int i = all.length - 22; i >= Math.max(0, all.length - 22 - 65535); i--) {
            if (b.getInt(i) == 0x06054b50) { eocd = i; break; }
        }
        if (eocd < 0) throw new IOException("To nie jest archiwum ZIP (brak EOCD)");
        int count = b.getShort(eocd + 10) & 0xFFFF;
        long cd = b.getInt(eocd + 16) & 0xFFFFFFFFL;
        List<Entry> out = new ArrayList<>();
        int p = (int) cd;
        for (int k = 0; k < count; k++) {
            if (b.getInt(p) != 0x02014b50) throw new IOException("Uszkodzony katalog centralny ZIP");
            Entry e = new Entry();
            e.method = b.getShort(p + 10) & 0xFFFF;
            int n = b.getShort(p + 28) & 0xFFFF;
            int x = b.getShort(p + 30) & 0xFFFF;
            int c = b.getShort(p + 32) & 0xFFFF;
            e.localOffset = b.getInt(p + 42) & 0xFFFFFFFFL;
            e.name = new String(all, p + 46, n, "UTF-8");
            int lo = (int) e.localOffset;
            if (b.getInt(lo) != 0x04034b50) throw new IOException("Uszkodzony nagłówek lokalny: " + e.name);
            int ln = b.getShort(lo + 26) & 0xFFFF;
            int lx = b.getShort(lo + 28) & 0xFFFF;
            e.dataOffset = e.localOffset + 30 + ln + lx;
            out.add(e);
            p += 46 + n + x + c;
        }
        return out;
    }

    static boolean verify(Path apk) throws IOException {
        boolean ok = true;
        boolean hasArsc = false;
        for (Entry e : entries(apk)) {
            if (e.name.equals(ARSC)) {
                hasArsc = true;
                if (e.method != ZipEntry.STORED) {
                    System.err.println("  BŁĄD: resources.arsc jest skompresowany (metoda " + e.method + ") - Android 11+ odrzuci APK [-124]");
                    ok = false;
                }
            }
            if (e.method == ZipEntry.STORED) {
                int a = e.name.endsWith(".so") ? SO_ALIGN : ALIGN;
                if (e.dataOffset % a != 0) {
                    System.err.println("  BŁĄD: " + e.name + " niewyrównany (offset " + e.dataOffset + ", wymagane " + a + " B)");
                    ok = false;
                }
            }
        }
        if (!hasArsc) System.err.println("  UWAGA: brak resources.arsc w " + apk);
        System.out.println(ok ? "  ApkCheck: OK (resources.arsc STORED, wyrównanie 4 B) - " + apk.getFileName()
                              : "  ApkCheck: APK NIEPOPRAWNY - " + apk.getFileName());
        return ok;
    }

    // ------------------------------------------------------------------ naprawa

    /** Licznik bajtów - pozycja w archiwum potrzebna do wyrównania. */
    static final class CountingOut extends FilterOutputStream {
        long count;
        CountingOut(OutputStream o) { super(o); }
        @Override public void write(int b) throws IOException { out.write(b); count++; }
        @Override public void write(byte[] b, int off, int len) throws IOException { out.write(b, off, len); count += len; }
    }

    static void fix(Path in, Path outPath) throws IOException {
        try (ZipFile zf = new ZipFile(in.toFile());
             CountingOut cnt = new CountingOut(new BufferedOutputStream(Files.newOutputStream(outPath)));
             ZipOutputStream zos = new ZipOutputStream(cnt)) {
            List<? extends ZipEntry> list = Collections.list(zf.entries());
            for (ZipEntry src : list) {
                byte[] data = readAll(zf.getInputStream(src));
                boolean stored = src.getName().equals(ARSC) || src.getMethod() == ZipEntry.STORED;
                ZipEntry dst = new ZipEntry(src.getName());
                dst.setTime(src.getTime());
                if (stored) {
                    CRC32 crc = new CRC32();
                    crc.update(data);
                    dst.setMethod(ZipEntry.STORED);
                    dst.setSize(data.length);
                    dst.setCompressedSize(data.length);
                    dst.setCrc(crc.getValue());
                    // pozycja nagłówka lokalnego = wszystko, co dotąd zapisano
                    zos.flush();
                    long header = cnt.count;
                    int nameLen = dst.getName().getBytes("UTF-8").length;
                    int a = dst.getName().endsWith(".so") ? SO_ALIGN : ALIGN;
                    long dataStart = header + 30 + nameLen + 6; // 6 = minimalne pole 0xD935
                    int pad = (int) ((a - (dataStart % a)) % a);
                    ByteBuffer extra = ByteBuffer.allocate(6 + pad).order(ByteOrder.LITTLE_ENDIAN);
                    extra.putShort((short) ALIGN_EXTRA_ID).putShort((short) (2 + pad)).putShort((short) a);
                    dst.setExtra(extra.array());
                } else {
                    dst.setMethod(ZipEntry.DEFLATED);
                }
                zos.putNextEntry(dst);
                zos.write(data);
                zos.closeEntry();
            }
        }
    }

    static byte[] readAll(InputStream is) throws IOException {
        try (InputStream in = is) {
            ByteArrayOutputStream bo = new ByteArrayOutputStream();
            byte[] buf = new byte[65536];
            int r;
            while ((r = in.read(buf)) > 0) bo.write(buf, 0, r);
            return bo.toByteArray();
        }
    }
}
