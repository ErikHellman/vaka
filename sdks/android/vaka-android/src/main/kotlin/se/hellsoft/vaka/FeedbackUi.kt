package se.hellsoft.vaka

import android.annotation.SuppressLint
import android.app.Activity
import android.app.AlertDialog
import android.app.Application
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.graphics.drawable.GradientDrawable
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.text.InputType
import android.util.TypedValue
import android.view.Choreographer
import android.view.Gravity
import android.view.PixelCopy
import android.view.View
import android.view.ViewGroup
import android.view.WindowManager
import android.widget.CheckBox
import android.widget.EditText
import android.widget.FrameLayout
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import android.widget.Toast
import java.io.ByteArrayOutputStream
import java.lang.ref.WeakReference

/** The resumed Activity, for [Vaka.showFeedback] without an argument. */
internal object CurrentActivity {
    @Volatile private var ref: WeakReference<Activity>? = null

    fun get(): Activity? = ref?.get()?.takeUnless { it.isFinishing }

    fun set(a: Activity?) {
        ref = a?.let { WeakReference(it) }
    }
}

/** Feedback dialog, floating button, screenshots and the crash prompt. Main thread only. */
internal object FeedbackUi : Application.ActivityLifecycleCallbacks {
    private val main = Handler(Looper.getMainLooper())
    private var options = VakaFeedbackOptions(button = false)
    val currentOptions: VakaFeedbackOptions get() = options
    private var registered = false
    private var dialog: AlertDialog? = null
    private var dialogActivity: WeakReference<Activity>? = null
    private const val MAX_SIDE = 1600
    private const val JPEG_QUALITY = 80

    fun install(app: Application, opts: VakaFeedbackOptions) {
        options = opts
        if (!registered) {
            app.registerActivityLifecycleCallbacks(this)
            registered = true
        }
        CurrentActivity.get()?.let { onActivityResumed(it) }
    }

    // ------------------------------------------------------------------ lifecycle

    override fun onActivityResumed(a: Activity) {
        updateButton(a)
        if (options.crashPrompt && dialog == null) {
            val crash = Vaka.takePreviousCrash() ?: return
            // Let the first frame draw before the dialog shows.
            main.post { if (!a.isFinishing) show(a, options, crash, crashPrompt = true) }
        }
    }

    override fun onActivityDestroyed(a: Activity) {
        if (dialogActivity?.get() === a) {
            dialog?.dismiss()
            dialog = null
        }
    }

    override fun onActivityCreated(a: Activity, b: Bundle?) {}
    override fun onActivityStarted(a: Activity) {}
    override fun onActivityPaused(a: Activity) {}
    override fun onActivityStopped(a: Activity) {}
    override fun onActivitySaveInstanceState(a: Activity, b: Bundle) {}

    // ------------------------------------------------------------------ button

    private fun updateButton(a: Activity) {
        val content = a.findViewById<ViewGroup>(android.R.id.content) as? FrameLayout ?: return
        val existing = content.findViewWithTag<View>(BUTTON_TAG)
        val wanted = options.button && a.javaClass !in options.excludeActivities
        if (!wanted) {
            existing?.let { content.removeView(it) }
            return
        }
        existing?.let { content.removeView(it) }
        val margin = dp(a, 16)
        val b = TextView(a).apply {
            tag = BUTTON_TAG
            text = options.strings.button
            contentDescription = options.strings.button
            setTextColor(Color.WHITE)
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 14f)
            setPadding(dp(a, 16), dp(a, 10), dp(a, 16), dp(a, 10))
            background = GradientDrawable().apply {
                setColor(options.accentColor)
                cornerRadius = dp(a, 24).toFloat()
            }
            elevation = dp(a, 6).toFloat()
            isClickable = true
            isFocusable = true
            setOnClickListener { show(a, options, null, crashPrompt = false) }
        }
        val lp = FrameLayout.LayoutParams(ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT,
            Gravity.BOTTOM or if (options.bottomLeft) Gravity.START else Gravity.END).apply { setMargins(margin, margin, margin, margin) }
        content.addView(b, lp)
        // Keep clear of the navigation bar when the content draws behind it (edge to edge).
        b.post {
            @Suppress("DEPRECATION")
            val inset = if (Build.VERSION.SDK_INT >= 23) content.rootWindowInsets?.systemWindowInsetBottom ?: 0 else 0
            val loc = IntArray(2).also { content.getLocationInWindow(it) }
            val below = a.window.decorView.height - (loc[1] + content.height)
            val extra = (inset - below).coerceAtLeast(0)
            if (extra > 0) {
                lp.bottomMargin = margin + extra
                b.layoutParams = lp
            }
        }
    }

    // ------------------------------------------------------------------ dialog

    fun show(activity: Activity, opts: VakaFeedbackOptions, eventId: String?, crashPrompt: Boolean) {
        if (dialog != null || activity.isFinishing) return
        if (opts.screenshot && !crashPrompt) {
            capture(activity) { shot -> if (!activity.isFinishing) open(activity, opts, eventId, crashPrompt, shot) }
        } else {
            open(activity, opts, eventId, crashPrompt, null)
        }
    }

    @SuppressLint("SetTextI18n")
    private fun open(a: Activity, opts: VakaFeedbackOptions, eventId: String?, crashPrompt: Boolean, shot: Bitmap?) {
        if (dialog != null) return
        val s = opts.strings
        val pad = dp(a, 20)
        val box = LinearLayout(a).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(pad, dp(a, 8), pad, 0)
        }
        if (crashPrompt) box.addView(TextView(a).apply { text = s.crashIntro; setPadding(0, 0, 0, dp(a, 8)) })
        val message = EditText(a).apply {
            hint = s.placeholder
            minLines = 3
            maxLines = 8
            gravity = Gravity.TOP or Gravity.START
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_MULTI_LINE or InputType.TYPE_TEXT_FLAG_CAP_SENTENCES
            filters = arrayOf(android.text.InputFilter.LengthFilter(5000))
        }
        box.addView(message)
        val email = if (opts.askEmail) {
            EditText(a).apply {
                hint = s.emailLabel
                setSingleLine()
                inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_EMAIL_ADDRESS
                opts.email?.let { setText(it) }
            }.also { box.addView(it) }
        } else {
            null
        }
        val include = if (shot != null) {
            val check = CheckBox(a).apply { text = s.screenshotLabel; isChecked = true }
            val thumb = ImageView(a).apply {
                setImageBitmap(shot)
                adjustViewBounds = true
                scaleType = ImageView.ScaleType.FIT_START
                maxHeight = dp(a, 160)
                contentDescription = s.screenshotLabel
                setOnClickListener { check.isChecked = !check.isChecked }
            }
            check.setOnCheckedChangeListener { _, on -> thumb.alpha = if (on) 1f else 0.3f }
            box.addView(check)
            box.addView(thumb, LinearLayout.LayoutParams(ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT))
            check
        } else {
            null
        }
        val status = TextView(a).apply { visibility = View.GONE; setTextColor(Color.rgb(0xDC, 0x26, 0x26)) }
        box.addView(status)

        val d = AlertDialog.Builder(a)
            .setTitle(if (crashPrompt) s.crashTitle else s.title)
            .setView(ScrollView(a).apply { addView(box) })
            .setPositiveButton(s.submit, null)
            .setNegativeButton(s.cancel, null)
            .create()
        d.setOnDismissListener { if (dialog === d) dialog = null }
        d.setOnShowListener {
            val send = d.getButton(AlertDialog.BUTTON_POSITIVE)
            send.setOnClickListener {
                val text = message.text.toString().trim()
                if (text.isEmpty()) {
                    message.error = s.messageRequired
                    return@setOnClickListener
                }
                send.isEnabled = false
                send.text = s.sending
                status.visibility = View.GONE
                val bitmap = shot?.takeIf { include?.isChecked == true }
                val emailText = email?.text?.toString()
                Thread({
                    val images = bitmap?.let { listOfNotNull(jpeg(it)) } ?: emptyList()
                    val fb = VakaFeedback(
                        message = text, email = emailText, eventId = eventId,
                        source = if (crashPrompt) VakaFeedbackSource.CRASH_PROMPT else VakaFeedbackSource.WIDGET,
                        attachments = images,
                    )
                    val sent = Vaka.captureFeedback(fb) { r ->
                        main.post {
                            if (r.status == VakaFeedbackResult.Status.REJECTED) {
                                send.isEnabled = true
                                send.text = s.submit
                                status.text = s.error
                                status.visibility = View.VISIBLE
                            } else {
                                Toast.makeText(a.applicationContext, s.thanks, Toast.LENGTH_SHORT).show()
                                d.dismiss()
                                opts.onSubmit?.invoke(r)
                            }
                        }
                    }
                    if (sent == null) main.post { d.dismiss() } // SDK not initialized
                }, "vaka-feedback").start()
            }
        }
        dialog = d
        dialogActivity = WeakReference(a)
        d.show()
    }

    // ------------------------------------------------------------------ screenshot

    /** Captures [a]'s window, masks sensitive views and calls back on the main thread. */
    private fun capture(a: Activity, done: (Bitmap?) -> Unit) {
        val window = a.window
        val decor = window.decorView
        if ((window.attributes.flags and WindowManager.LayoutParams.FLAG_SECURE) != 0 || decor.width == 0 || decor.height == 0) {
            done(null)
            return
        }
        val scale = minOf(1f, MAX_SIDE.toFloat() / maxOf(decor.width, decor.height))
        val w = (decor.width * scale).toInt().coerceAtLeast(1)
        val h = (decor.height * scale).toInt().coerceAtLeast(1)
        val button = (a.findViewById<ViewGroup>(android.R.id.content))?.findViewWithTag<View>(BUTTON_TAG)
        val wasVisible = button?.visibility
        val bitmap = try {
            Bitmap.createBitmap(w, h, Bitmap.Config.ARGB_8888)
        } catch (e: OutOfMemoryError) {
            done(null)
            return
        }
        fun finish(ok: Boolean) {
            if (button != null && wasVisible != null) button.visibility = wasVisible
            if (!ok) {
                bitmap.recycle()
                done(null)
                return
            }
            mask(decor, bitmap, scale)
            done(bitmap)
        }
        fun draw(): Boolean = try {
            button?.visibility = View.INVISIBLE
            Canvas(bitmap).apply { scale(scale, scale) }.also { decor.draw(it) }
            true
        } catch (e: Exception) {
            false
        }
        if (Build.VERSION.SDK_INT >= 26) {
            // PixelCopy reads what is on screen, so hide the button and wait two frames first.
            button?.visibility = View.INVISIBLE
            afterFrames(2) {
                try {
                    PixelCopy.request(window, bitmap, { result ->
                        finish(result == PixelCopy.SUCCESS || draw())
                    }, main)
                } catch (e: Exception) {
                    finish(draw())
                }
            }
        } else {
            finish(draw())
        }
    }

    private fun afterFrames(n: Int, r: () -> Unit) {
        if (n <= 0) r() else Choreographer.getInstance().postFrameCallback { afterFrames(n - 1, r) }
    }

    /** Blacks out password fields and views marked with [Vaka.maskInScreenshots]. */
    private fun mask(root: View, bitmap: Bitmap, scale: Float) {
        val canvas = Canvas(bitmap)
        val paint = Paint().apply { color = Color.BLACK }
        val loc = IntArray(2)
        fun visit(v: View) {
            if (v.visibility != View.VISIBLE) return
            if (isSensitive(v)) {
                v.getLocationInWindow(loc)
                canvas.drawRect(loc[0] * scale, loc[1] * scale, (loc[0] + v.width) * scale, (loc[1] + v.height) * scale, paint)
                return
            }
            if (v is ViewGroup) for (i in 0 until v.childCount) visit(v.getChildAt(i))
        }
        visit(root)
    }

    private fun isSensitive(v: View): Boolean {
        if (v.getTag(R.id.vaka_mask) == true) return true
        if (v is TextView) {
            val t = v.inputType
            val cls = t and InputType.TYPE_MASK_CLASS
            val variation = t and InputType.TYPE_MASK_VARIATION
            if (cls == InputType.TYPE_CLASS_TEXT && (variation == InputType.TYPE_TEXT_VARIATION_PASSWORD ||
                    variation == InputType.TYPE_TEXT_VARIATION_WEB_PASSWORD || variation == InputType.TYPE_TEXT_VARIATION_VISIBLE_PASSWORD)) return true
            if (cls == InputType.TYPE_CLASS_NUMBER && variation == InputType.TYPE_NUMBER_VARIATION_PASSWORD) return true
            if (v.transformationMethod is android.text.method.PasswordTransformationMethod) return true
        }
        return false
    }

    /** JPEG of at most 5 MB, lowering the quality if needed. */
    private fun jpeg(b: Bitmap): ByteArray? {
        var q = JPEG_QUALITY
        while (q >= 30) {
            val out = ByteArrayOutputStream()
            if (!b.compress(Bitmap.CompressFormat.JPEG, q, out)) return null
            if (out.size() <= 5 * 1024 * 1024) return out.toByteArray()
            q -= 20
        }
        return null
    }

    private fun dp(a: Activity, v: Int): Int = (v * a.resources.displayMetrics.density + 0.5f).toInt()

    private const val BUTTON_TAG = "se.hellsoft.vaka.feedback.button"
}
