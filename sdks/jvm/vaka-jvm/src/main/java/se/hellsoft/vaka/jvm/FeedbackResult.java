package se.hellsoft.vaka.jvm;

/** The stored feedback's ids. */
public final class FeedbackResult {
    public final String id;
    /** Looks like {@code API-F12}. */
    public final String shortId;

    public FeedbackResult(String id, String shortId) {
        this.id = id;
        this.shortId = shortId;
    }

    @Override
    public String toString() { return "FeedbackResult{id=" + id + ", shortId=" + shortId + "}"; }
}
