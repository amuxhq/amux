package sh.amux.features

import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material.icons.outlined.Description
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import java.io.File
import sh.amux.design.AmuxHairline
import sh.amux.design.AmuxTheme
import sh.amux.design.Space
import sh.amux.values.Row

/** Attachment preview currently opened in a full-screen dialog. */
@Immutable
data class ChatOpenedAttachmentUi(
    val name: String,
    val bytes: ByteArray? = null,
    val isImage: Boolean = true,
    val file: File? = null,
) {
    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is ChatOpenedAttachmentUi) return false
        return name == other.name &&
            bytes.contentEquals(other.bytes) &&
            isImage == other.isImage &&
            file == other.file
    }

    override fun hashCode(): Int {
        var result = name.hashCode()
        result = 31 * result + (bytes?.contentHashCode() ?: 0)
        result = 31 * result + isImage.hashCode()
        result = 31 * result + (file?.hashCode() ?: 0)
        return result
    }
}

@Composable
internal fun AttachmentPreviewDialog(
    attachment: ChatOpenedAttachmentUi,
    onOpenExternally: (File) -> Unit,
    onShare: (File) -> Unit,
    onDismiss: () -> Unit,
) {
    val colors = AmuxTheme.colors
    val closeLabel = stringResource(R.string.action_close)
    val bitmap = rememberDecodedImage(bytes = attachment.bytes, maxPx = rememberScreenMaxPx()).bitmapOrNull
    val localFile = attachment.file

    Dialog(
        onDismissRequest = onDismiss,
        properties = DialogProperties(usePlatformDefaultWidth = false),
    ) {
        Column(
            modifier = Modifier
                .fillMaxSize()
                .background(colors.ground)
                .navigationBarsPadding()
                .testTag(ChatScreenTags.PREVIEW_DIALOG),
        ) {
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = Space.m, vertical = Space.s),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(Space.s),
            ) {
                Text(
                    text = attachment.name,
                    style = AmuxTheme.typography.bodyEmphasis,
                    color = colors.ink,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.weight(1f),
                )
                IconButton(
                    onClick = onDismiss,
                    modifier = Modifier.testTag(ChatScreenTags.PREVIEW_CLOSE),
                ) {
                    Icon(
                        imageVector = Icons.Outlined.Close,
                        contentDescription = closeLabel,
                        tint = colors.ink,
                    )
                }
            }
            AmuxHairline()
            Box(
                modifier = Modifier
                    .weight(1f)
                    .fillMaxWidth()
                    .padding(Space.l),
                contentAlignment = Alignment.Center,
            ) {
                if (bitmap != null) {
                    Image(
                        bitmap = bitmap,
                        contentDescription = attachment.name,
                        contentScale = ContentScale.Fit,
                        modifier = Modifier.fillMaxSize(),
                    )
                } else {
                    Column(
                        horizontalAlignment = Alignment.CenterHorizontally,
                        verticalArrangement = Arrangement.spacedBy(Space.m),
                    ) {
                        Icon(
                            imageVector = Icons.Outlined.Description,
                            contentDescription = null,
                            tint = colors.inkMuted,
                            modifier = Modifier.size(40.dp),
                        )
                        Text(
                            text = attachment.name,
                            style = AmuxTheme.typography.mono,
                            color = colors.ink,
                        )
                        Text(
                            text = stringResource(R.string.chat_preview_unavailable),
                            style = AmuxTheme.typography.detail,
                            color = colors.inkMuted,
                            textAlign = TextAlign.Center,
                        )
                        if (localFile != null) {
                            Row(
                                horizontalArrangement = Arrangement.spacedBy(Space.s),
                                verticalAlignment = Alignment.CenterVertically,
                            ) {
                                AmuxActionButton(
                                    label = stringResource(R.string.chat_preview_open),
                                    kind = ActionButtonKind.Primary,
                                    onClick = { onOpenExternally(localFile) },
                                    modifier = Modifier.testTag(ChatScreenTags.PREVIEW_OPEN),
                                )
                                AmuxActionButton(
                                    label = stringResource(R.string.chat_preview_share),
                                    kind = ActionButtonKind.Outline,
                                    onClick = { onShare(localFile) },
                                    modifier = Modifier.testTag(ChatScreenTags.PREVIEW_SHARE),
                                )
                            }
                        }
                    }
                }
            }
        }
    }
}
