package se.hellsoft.vaka.jvm;

/** Thrown by {@link Vaka#captureFeedback(Feedback)} when the feedback was not stored. */
public final class FeedbackException extends RuntimeException {
    private final int status;

    public FeedbackException(String message, int status) {
        super(message);
        this.status = status;
    }

    /** The HTTP status, or 0 when nothing was sent. 429 means a rate limit was hit. */
    public int status() { return status; }
}
