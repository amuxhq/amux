package sh.amux.features

import sh.amux.design.IconSize
import sh.amux.design.AmuxShapes
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.Image
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.Notes
import androidx.compose.material.icons.outlined.AttachFile
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material.icons.outlined.Description
import androidx.compose.material.icons.outlined.Image
import androidx.compose.material.icons.outlined.Language
import androidx.compose.material.icons.outlined.RateReview
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import sh.amux.design.AmuxTheme
import sh.amux.design.Space
import sh.amux.design.thumbTarget
import sh.amux.values.AttachmentView
import sh.amux.values.BlobRef
import sh.amux.values.Row
import sh.amux.words.ChatWords

@Composable
internal fun MessageAttachmentView(
    view: AttachmentView,
    imageBytes: (BlobRef) -> ByteArray?,
    onOpenImage: (List<Int>, String) -> Unit,
    onOpenAttachment: (AttachmentView) -> Unit,
) {
    when (view) {
        is AttachmentView.Image -> {
            if (view.value.size <= RowGridMetrics.THUMBNAIL_SIZE_LIMIT) {
                ImageThumbnailCard(
                    blob = view.value,
                    label = view.value.name,
                    imageBytes = imageBytes,
                    named = true,
                    onOpen = { onOpenImage(view.value.hash, view.value.name) },
                )
            } else {
                AttachmentChip(
                    view = view,
                    onClick = { onOpenAttachment(view) },
                )
            }
        }
        is AttachmentView.File -> {
            AttachmentChip(
                view = view,
                onClick = { onOpenAttachment(view) },
            )
        }
        else -> {
            AttachmentChip(
                view = view,
                onClick = null,
            )
        }
    }
}

/**
 * Shared attachment chip used in both prompt bubbles and [ChatComposer].
 */
@Composable
fun AttachmentChip(
    view: AttachmentView,
    modifier: Modifier = Modifier,
    thumbnailBytes: ByteArray? = null,
    onClick: (() -> Unit)? = null,
    onRemove: (() -> Unit)? = null,
) {
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography
    val metrics = AmuxTheme.metrics
    val parts = remember(view) { ChatWords.chipParts(view) }
    val chipLabel = remember(view) { ChatWords.chip(view) }
    val bitmap = rememberDecodedImage(
        bytes = thumbnailBytes,
        maxPx = rememberPx(24.dp),
        key = (view as? AttachmentView.Image)?.value?.hash,
        crop = true,
    ).bitmapOrNull
    val openDescription = stringResource(R.string.chat_row_attachment_open_named, chipLabel)
    val removeDescription = stringResource(R.string.chat_row_attachment_remove)

    val containerModifier = if (onClick != null) {
        modifier
            .thumbTarget()
            .clip(AmuxShapes.Pill)
            .clickable(role = Role.Button, onClick = onClick)
            .semantics { contentDescription = openDescription }
    } else {
        modifier.semantics { contentDescription = chipLabel }
    }

    Surface(
        modifier = containerModifier.testTag(ChatRowTags.ATTACHMENT_CHIP),
        shape = AmuxShapes.Pill,
        color = colors.raised,
        contentColor = colors.inkMuted,
        border = BorderStroke(metrics.hairline, colors.hairline),
    ) {
        Row(
            modifier = Modifier.padding(
                start = Space.s + Space.xxs,
                end = if (onRemove != null) Space.xxs else Space.s + Space.xxs,
                top = Space.xs,
                bottom = Space.xs,
            ),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Space.xs + Space.xxs),
        ) {
            if (bitmap != null) {
                Image(
                    bitmap = bitmap,
                    contentDescription = null,
                    contentScale = ContentScale.Crop,
                    modifier = Modifier
                        .size(24.dp)
                        .clip(RoundedCornerShape(6.dp)),
                )
            } else {
                Icon(
                    imageVector = attachmentKindIcon(ChatWords.chipKind(view)),
                    contentDescription = null,
                    tint = colors.inkMuted,
                    modifier = Modifier.size(IconSize.Small),
                )
            }
            Text(
                text = chipLabel,
                style = typography.caption,
                color = colors.inkMuted,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.widthIn(max = 220.dp),
            )
            if (onRemove != null) {
                IconButton(
                    onClick = onRemove,
                    modifier = Modifier.testTag(ChatRowTags.ATTACHMENT_REMOVE),
                ) {
                    Icon(
                        imageVector = Icons.Outlined.Close,
                        contentDescription = removeDescription,
                        tint = colors.inkFaint,
                        modifier = Modifier.size(IconSize.Inline),
                    )
                }
            }
        }
    }
}

internal fun attachmentKindIcon(kind: ChatWords.AttachmentKind): ImageVector = when (kind) {
    ChatWords.AttachmentKind.Image -> Icons.Outlined.Image
    ChatWords.AttachmentKind.File -> Icons.Outlined.Description
    ChatWords.AttachmentKind.Text -> Icons.AutoMirrored.Outlined.Notes
    ChatWords.AttachmentKind.Review -> Icons.Outlined.RateReview
    ChatWords.AttachmentKind.Page -> Icons.Outlined.Language
    ChatWords.AttachmentKind.Empty -> Icons.Outlined.AttachFile
}

@Composable
internal fun ImageThumbnailCard(
    blob: BlobRef,
    label: String,
    imageBytes: (BlobRef) -> ByteArray?,
    named: Boolean,
    onOpen: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography
    val metrics = AmuxTheme.metrics
    // The row hands over a new [imageBytes] when the blob's bytes land, so
    // that is what tells the image to look again.
    val image = rememberDecodedImage(
        key = blob.hash,
        maxPx = rememberPx(240.dp),
        revision = imageBytes,
    ) { imageBytes(blob) }
    val bitmap = image.bitmapOrNull
    val openDescription = stringResource(R.string.chat_row_image_open_named, label)
    val caption = remember(blob) { ChatWords.chip(AttachmentView.Image(blob)) }

    Column(
        modifier = modifier
            .thumbTarget()
            .clip(RoundedCornerShape(metrics.controlRadius))
            .clickable(role = Role.Button, onClick = onOpen)
            .semantics { contentDescription = openDescription }
            .testTag(ChatRowTags.IMAGE_THUMBNAIL),
        verticalArrangement = Arrangement.spacedBy(Space.xs),
    ) {
        Surface(
            modifier = Modifier.size(width = 240.dp, height = 170.dp),
            shape = RoundedCornerShape(metrics.controlRadius),
            color = colors.sunken,
            border = BorderStroke(metrics.hairline, colors.hairline),
        ) {
            if (bitmap != null) {
                Image(
                    bitmap = bitmap,
                    contentDescription = null,
                    contentScale = ContentScale.Fit,
                )
            } else {
                Column(
                    horizontalAlignment = Alignment.CenterHorizontally,
                    verticalArrangement = Arrangement.Center,
                    modifier = Modifier.padding(Space.m),
                ) {
                    Icon(
                        imageVector = Icons.Outlined.Image,
                        contentDescription = null,
                        tint = colors.inkFaint,
                        modifier = Modifier.size(28.dp),
                    )
                    if (image == DecodedImage.Unreadable) {
                        Spacer(modifier = Modifier.height(Space.xs))
                        Text(
                            text = stringResource(R.string.chat_row_image_no_preview),
                            style = typography.caption,
                            color = colors.inkFaint,
                        )
                    }
                }
            }
        }
        if (named) {
            Text(
                text = caption,
                style = typography.caption,
                color = colors.inkMuted,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.widthIn(max = 240.dp),
            )
        }
    }
}

@Composable
internal fun ImagePlaceholderCard(
    label: String,
    onOpen: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography
    val metrics = AmuxTheme.metrics
    val openDescription = stringResource(R.string.chat_row_image_open_named, label)

    Surface(
        modifier = modifier
            .thumbTarget()
            .clip(RoundedCornerShape(metrics.controlRadius))
            .clickable(role = Role.Button, onClick = onOpen)
            .semantics { contentDescription = openDescription }
            .testTag(ChatRowTags.IMAGE_THUMBNAIL),
        shape = RoundedCornerShape(metrics.controlRadius),
        color = colors.sunken,
        border = BorderStroke(metrics.hairline, colors.hairline),
    ) {
        Row(
            modifier = Modifier.padding(horizontal = Space.m, vertical = Space.s),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Space.s),
        ) {
            Icon(
                imageVector = Icons.Outlined.Image,
                contentDescription = null,
                tint = colors.inkMuted,
                modifier = Modifier.size(IconSize.Control),
            )
            Text(
                text = label,
                style = typography.caption,
                color = colors.inkMuted,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
    }
}
