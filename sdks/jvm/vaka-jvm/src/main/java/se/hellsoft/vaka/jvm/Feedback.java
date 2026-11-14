package se.hellsoft.vaka.jvm;

import java.util.ArrayList;
import java.util.Collections;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Objects;

/**
 * Feedback a user wrote (a support form, in-app chat), sent with {@link Vaka#captureFeedback(Feedback)}.
 *
 * <pre>{@code
 * FeedbackResult r = Vaka.captureFeedback(Feedback.of(form.message())
 *     .email(form.email())
 *     .issue("API-12")
 *     .ip(request.getRemoteAddr()));
 * }</pre>
 */
public final class Feedback {
    /** Most images one feedback item can carry. */
    public static final int MAX_ATTACHMENTS = 3;

    /** An image sent with feedback: PNG, JPEG or WebP, at most 5 MB. */
    public static final class Attachment {
        public final String filename;
        public final String contentType;
        public final byte[] data;

        Attachment(String filename, String contentType, byte[] data) {
            this.filename = filename;
            this.contentType = contentType;
            this.data = data;
        }
    }

    final String message;
    String email;
    String name;
    String eventId;
    String issue;
    String userId;
    String sessionId;
    String url;
    String ip;
    final Map<String, String> tags = new LinkedHashMap<>();
    final List<Attachment> attachments = new ArrayList<>();

    private Feedback(String message) { this.message = message; }

    /** Required: what the user wrote. Longer than 5000 characters is cut. */
    public static Feedback of(String message) { return new Feedback(Objects.requireNonNull(message, "message")); }

    /** Contact email, only when the user gave it. */
    public Feedback email(String email) { this.email = email; return this; }

    public Feedback name(String name) { this.name = name; return this; }

    /** The error event this feedback is about (the id {@link Vaka#captureException(Throwable)} returned). */
    public Feedback eventId(String eventId) { this.eventId = eventId; return this; }

    /** The issue this feedback is about: its id or short id like {@code API-12}. */
    public Feedback issue(String issue) { this.issue = issue; return this; }

    /** Defaults to the current scope's user. */
    public Feedback userId(String userId) { this.userId = userId; return this; }

    /** The end user's session, when your app has one. */
    public Feedback sessionId(String sessionId) { this.sessionId = sessionId; return this; }

    /** Page or screen the feedback is about. */
    public Feedback url(String url) { this.url = url; return this; }

    /**
     * The end user's IP address. Feedback is rate limited per address, so without it all feedback
     * from this server shares one limit.
     */
    public Feedback ip(String ip) { this.ip = ip; return this; }

    public Feedback tag(String key, String value) {
        tags.put(Objects.requireNonNull(key), Objects.requireNonNull(value));
        return this;
    }

    /** Adds an image; the content type is guessed from the file name. */
    public Feedback attachment(String filename, byte[] data) { return attachment(filename, data, null); }

    public Feedback attachment(String filename, byte[] data, String contentType) {
        attachments.add(new Attachment(Objects.requireNonNull(filename), contentType != null ? contentType : contentTypeOf(filename), Objects.requireNonNull(data)));
        return this;
    }

    public List<Attachment> attachments() { return Collections.unmodifiableList(attachments); }

    static String contentTypeOf(String filename) {
        String f = filename.toLowerCase(java.util.Locale.ROOT);
        if (f.endsWith(".png")) return "image/png";
        if (f.endsWith(".webp")) return "image/webp";
        return "image/jpeg";
    }
}
