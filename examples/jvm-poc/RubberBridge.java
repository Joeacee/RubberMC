public final class RubberBridge {
    private RubberBridge() {}

    public static String greeting(String name) {
        return "Hello from Java, " + name;
    }

    public static native int rustAdd(int left, int right);

    public static void throwForTest() {
        throw new IllegalStateException("expected proof-of-concept exception");
    }
}
