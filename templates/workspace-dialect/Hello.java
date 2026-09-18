package forge.app;

import forge.Op;
import forge.OpContext;
import forge.Value;

public final class Hello {

    public record Greeting(String message) {}

    @Op("hello")
    public static Greeting hello(OpContext ctx, Value input) {
        String name = input.get("name", "world");
        return new Greeting("hello, " + name + ", from {{domain}}");
    }
}
