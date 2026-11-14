package se.hellsoft.vaka

/**
 * User feedback for [Vaka.captureFeedback].
 *
 * @property message what the user wrote (required, at most 5000 characters).
 * @property email contact email, only when the user typed it or agreed to share it.
 * @property name contact name.
 * @property eventId the error event this feedback is about, e.g. [Vaka.lastEventId]. Vaka links
 *   the feedback to that event's issue.
 * @property source where it came from: [VakaFeedbackSource.API] (default), the widget or the crash prompt.
 * @property tags extra tags, merged with the tags set with [Vaka.setTag].
 * @property attachments up to 3 PNG, JPEG or WebP images, 5 MB each. Others are dropped.
 */
public data class VakaFeedback @JvmOverloads constructor(
    val message: String,
    val email: String? = null,
    val name: String? = null,
    val eventId: String? = null,
    val source: VakaFeedbackSource = VakaFeedbackSource.API,
    val tags: Map<String, String> = emptyMap(),
    val attachments: List<ByteArray> = emptyList(),
)

public enum class VakaFeedbackSource(internal val wire: String) {
    WIDGET("widget"), CRASH_PROMPT("crash_prompt"), API("api"),
}

/**
 * Outcome of [Vaka.captureFeedback].
 *
 * @property status [Status.SENT] (stored, [shortId] set), [Status.QUEUED] (offline; it is sent
 *   later, also after a restart) or [Status.REJECTED] (invalid or rate limited; [error] says why).
 * @property id the feedback's client id.
 * @property shortId the server's short id, e.g. `AND-F12`, once sent.
 */
public data class VakaFeedbackResult(
    val status: Status,
    val id: String,
    val shortId: String? = null,
    val error: String? = null,
) {
    public enum class Status { SENT, QUEUED, REJECTED }
}

/**
 * Options for [Vaka.installFeedback] and [Vaka.showFeedback].
 *
 * @property button show a floating feedback button on every Activity.
 * @property crashPrompt after the app crashed, ask "what were you doing?" on the next launch. The
 *   answer is linked to that crash. Covers JVM and native crashes.
 * @property screenshot attach a screenshot of the current screen; the user can remove it.
 *   Password fields and views passed to [Vaka.maskInScreenshots] are blacked out, and windows
 *   with `FLAG_SECURE` are never captured.
 * @property askEmail show the optional email field.
 * @property email prefill the email field, e.g. for signed-in users.
 * @property accentColor button color (ARGB).
 * @property bottomLeft put the button in the bottom-left corner instead of bottom-right.
 * @property excludeActivities Activities that get no button (e.g. a splash or checkout screen).
 * @property strings texts shown to the user.
 * @property onSubmit called on the main thread after feedback was sent or queued.
 */
public data class VakaFeedbackOptions @JvmOverloads constructor(
    val button: Boolean = true,
    val crashPrompt: Boolean = false,
    val screenshot: Boolean = true,
    val askEmail: Boolean = true,
    val email: String? = null,
    val accentColor: Int = 0xFF2563EB.toInt(),
    val bottomLeft: Boolean = false,
    val excludeActivities: Set<Class<*>> = emptySet(),
    val strings: VakaFeedbackStrings = VakaFeedbackStrings(),
    val onSubmit: ((VakaFeedbackResult) -> Unit)? = null,
)

/** Texts of the feedback widget. Override any of them to translate or reword. */
public data class VakaFeedbackStrings @JvmOverloads constructor(
    val button: String = "Feedback",
    val title: String = "Send feedback",
    val crashTitle: String = "Something went wrong",
    val crashIntro: String = "Sorry, the app crashed. What were you doing when it happened?",
    val placeholder: String = "What's on your mind? Tell us what happened or what you'd like to see.",
    val emailLabel: String = "Email (optional, if you want a reply)",
    val screenshotLabel: String = "Include screenshot",
    val submit: String = "Send",
    val cancel: String = "Cancel",
    val sending: String = "Sending…",
    val thanks: String = "Thanks! Your feedback was sent.",
    val error: String = "Could not send your feedback. Please try again.",
    val messageRequired: String = "Please write a message.",
)
