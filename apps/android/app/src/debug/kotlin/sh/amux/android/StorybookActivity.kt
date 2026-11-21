package sh.amux.android

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.BackHandler
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.outlined.DarkMode
import androidx.compose.material.icons.outlined.LightMode
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.Density
import androidx.compose.ui.unit.dp
import androidx.compose.foundation.layout.heightIn
import sh.amux.design.AmuxGround
import sh.amux.design.AmuxHairline
import sh.amux.design.AmuxTheme
import sh.amux.design.Appearance
import sh.amux.design.Space
import sh.amux.features.AmuxRowCard
import sh.amux.features.AmuxSectionHeader
import sh.amux.testsupport.Stories
import sh.amux.testsupport.Story

/**
 * Interactive debug-only launcher listing [Stories.all] with a Light/Dark
 * toggle for live inspection on emulators or physical devices.
 *
 * Registered exclusively in `app/src/debug/AndroidManifest.xml` and never
 * linked into release builds.
 */
class StorybookActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        val initialStoryId = intent?.getStringExtra(EXTRA_STORY_ID)
        setContent {
            StorybookBrowser(initialStoryId = initialStoryId)
        }
    }

    companion object {
        const val EXTRA_STORY_ID: String = "story"
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun StorybookBrowser(initialStoryId: String?) {
    var appearance by remember { mutableStateOf(Appearance.Light) }
    var selected by remember {
        mutableStateOf(
            initialStoryId?.let { id -> Stories.all.firstOrNull { it.id == id } },
        )
    }

    BackHandler(enabled = selected != null) {
        selected = null
    }

    AmuxTheme(appearance = appearance) {
        val current = selected
        if (current != null) {
            val baseDensity = LocalDensity.current
            val scaledDensity = Density(
                density = baseDensity.density,
                fontScale = baseDensity.fontScale * current.fontScale,
            )
            Column(modifier = Modifier.fillMaxSize()) {
                TopAppBar(
                    colors = TopAppBarDefaults.topAppBarColors(
                        containerColor = AmuxTheme.colors.raised,
                        titleContentColor = AmuxTheme.colors.ink,
                    ),
                    navigationIcon = {
                        IconButton(
                            onClick = { selected = null },
                        ) {
                            Icon(
                                imageVector = Icons.AutoMirrored.Outlined.ArrowBack,
                                contentDescription = "Back",
                            )
                        }
                    },
                    title = {
                        Text(
                            text = current.id,
                            style = AmuxTheme.typography.monoSmall,
                            color = AmuxTheme.colors.ink,
                        )
                    },
                    actions = {
                        AppearanceToggleButton(
                            appearance = appearance,
                            onToggle = {
                                appearance = if (appearance == Appearance.Light) {
                                    Appearance.Dark
                                } else {
                                    Appearance.Light
                                }
                            },
                        )
                    },
                )
                AmuxHairline()
                Box(modifier = Modifier.weight(1f)) {
                    CompositionLocalProvider(LocalDensity provides scaledDensity) {
                        current.content()
                    }
                }
            }
        } else {
            val grouped = remember { Stories.all.groupBy { it.area } }
            AmuxGround {
                Scaffold(
                    containerColor = Color.Transparent,
                    contentColor = AmuxTheme.colors.ink,
                    topBar = {
                        Column {
                            TopAppBar(
                                colors = TopAppBarDefaults.topAppBarColors(
                                    containerColor = AmuxTheme.colors.ground,
                                    titleContentColor = AmuxTheme.colors.ink,
                                ),
                                title = {
                                    Text(
                                        text = "Storybook",
                                        style = AmuxTheme.typography.screenTitle,
                                    )
                                },
                                actions = {
                                    AppearanceToggleButton(
                                        appearance = appearance,
                                        onToggle = {
                                            appearance = if (appearance == Appearance.Light) {
                                                Appearance.Dark
                                            } else {
                                                Appearance.Light
                                            }
                                        },
                                    )
                                },
                            )
                            AmuxHairline()
                        }
                    },
                ) { innerPadding ->
                    LazyColumn(
                        modifier = Modifier.fillMaxSize(),
                        contentPadding = PaddingValues(
                            top = innerPadding.calculateTopPadding() + Space.s,
                            bottom = innerPadding.calculateBottomPadding() + Space.xl,
                        ),
                        verticalArrangement = Arrangement.spacedBy(Space.m),
                    ) {
                        for ((area, stories) in grouped) {
                            item(key = "area-$area") {
                                Column {
                                    AmuxSectionHeader(
                                        title = area,
                                        trailing = stories.size.toString(),
                                    )
                                    AmuxRowCard(modifier = Modifier.padding(horizontal = Space.margin)) {
                                        stories.forEachIndexed { index, story ->
                                            StoryRow(
                                                story = story,
                                                onClick = { selected = story },
                                            )
                                            if (index < stories.lastIndex) {
                                                AmuxHairline(
                                                    modifier = Modifier.padding(horizontal = Space.l),
                                                )
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun AppearanceToggleButton(
    appearance: Appearance,
    onToggle: () -> Unit,
) {
    IconButton(onClick = onToggle) {
        Icon(
            imageVector = if (appearance == Appearance.Light) {
                Icons.Outlined.DarkMode
            } else {
                Icons.Outlined.LightMode
            },
            contentDescription = if (appearance == Appearance.Light) "Dark theme" else "Light theme",
            tint = AmuxTheme.colors.ink,
        )
    }
}

@Composable
private fun StoryRow(
    story: Story,
    onClick: () -> Unit,
) {
    val colors = AmuxTheme.colors
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .heightIn(min = 56.dp)
            .clickable(onClick = onClick)
            .padding(horizontal = Space.l, vertical = Space.m),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(
            modifier = Modifier.weight(1f),
            verticalArrangement = Arrangement.spacedBy(Space.xxs),
        ) {
            Text(
                text = story.title,
                style = AmuxTheme.typography.identifier,
                color = colors.ink,
            )
            Text(
                text = story.id,
                style = AmuxTheme.typography.monoSmall,
                color = colors.inkMuted,
            )
        }
    }
}
