package forge.app;

import forge.Op;
import forge.OpContext;
import forge.schema.inputs.{{domain}}.HelloRequest;
import forge.schema.inputs.{{domain}}.HelloResponse;

/** The {@code {{domain}}::hello} starter op. */
public final class Hello {

  /** Greets the caller by the name they sent. */
  @Op
  public static HelloResponse hello(OpContext ctx, HelloRequest req) {
    return new HelloResponse("hello, " + req.name() + ", from {{domain}}");
  }
}
