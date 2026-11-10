package se.hellsoft.vaka.gradle

import com.android.build.api.artifact.SingleArtifact
import com.android.build.api.variant.ApplicationAndroidComponentsExtension
import org.gradle.api.Plugin
import org.gradle.api.Project
import org.gradle.api.provider.Provider

/**
 * Registers `uploadVakaMapping<Variant>` for every minified application variant and runs it after
 * `minify<Variant>WithR8`, so every release build uploads the mapping that matches it.
 */
class VakaPlugin : Plugin<Project> {
    override fun apply(project: Project) {
        val ext = project.extensions.create("vaka", VakaExtension::class.java)
        ext.url.convention(setting(project, "VAKA_URL", "vaka.url"))
        ext.project.convention(setting(project, "VAKA_PROJECT", "vaka.project"))
        ext.token.convention(setting(project, "VAKA_TOKEN", "vaka.token"))
        ext.uploadMapping.convention(
            setting(project, "VAKA_UPLOAD_MAPPING", "vaka.uploadMapping").map { it.toBoolean() }.orElse(true),
        )
        ext.failOnError.convention(false)

        project.pluginManager.withPlugin("com.android.application") {
            val components = project.extensions.getByType(ApplicationAndroidComponentsExtension::class.java)
            components.onVariants { variant ->
                if (!variant.isMinifyEnabled) return@onVariants
                val cap = variant.name.replaceFirstChar { it.uppercase() }
                val versionName = variant.outputs.firstOrNull()?.versionName
                val upload = project.tasks.register("uploadVakaMapping$cap", UploadMappingTask::class.java) {
                    val t = this
                    t.group = "vaka"
                    t.description = "Uploads the R8 mapping file of the ${variant.name} variant to Vaka."
                    t.mappingFile.set(variant.artifacts.get(SingleArtifact.OBFUSCATION_MAPPING_FILE))
                    t.serverUrl.set(ext.url)
                    t.projectId.set(ext.project)
                    t.token.set(ext.token)
                    t.release.set(ext.release.orElse(versionName ?: project.provider { "" }))
                    t.failOnError.set(ext.failOnError)
                    t.onlyIf { ext.uploadMapping.get() }
                }
                project.tasks.matching { it.name == "minify${cap}WithR8" }.configureEach { finalizedBy(upload) }
            }
        }
    }

    private fun setting(project: Project, env: String, prop: String): Provider<String> =
        project.providers.gradleProperty(prop).orElse(project.providers.environmentVariable(env))
}
