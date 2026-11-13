package se.hellsoft.vaka.jvm;

import io.opentelemetry.api.common.AttributeKey;
import io.opentelemetry.context.Context;
import io.opentelemetry.sdk.trace.ReadWriteSpan;
import io.opentelemetry.sdk.trace.ReadableSpan;
import io.opentelemetry.sdk.trace.SpanProcessor;

/**
 * Adds {@code enduser.id} (from the current {@link VakaScope}) and {@code session.id} to every
 * span, so Vaka links traces to users and release-health sessions.
 */
final class VakaSpanProcessor implements SpanProcessor {
    static final AttributeKey<String> ENDUSER_ID = AttributeKey.stringKey("enduser.id");
    static final AttributeKey<String> SESSION_ID = AttributeKey.stringKey("session.id");

    private final VakaClient client;

    VakaSpanProcessor(VakaClient client) { this.client = client; }

    @Override
    public void onStart(Context parentContext, ReadWriteSpan span) {
        VakaScope scope = VakaScope.fromContext(parentContext);
        String user = scope != null && scope.user() != null ? scope.user() : client.globalScope().user();
        if (user != null) span.setAttribute(ENDUSER_ID, user);
        String session = client.sessionId();
        if (session != null) span.setAttribute(SESSION_ID, session);
    }

    @Override
    public boolean isStartRequired() { return true; }

    @Override
    public void onEnd(ReadableSpan span) {}

    @Override
    public boolean isEndRequired() { return false; }
}
