package sh.amux.features

import sh.amux.design.IconSize
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.KeyboardArrowDown
import androidx.compose.material3.Checkbox
import androidx.compose.material3.CheckboxDefaults
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Icon
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.input.KeyboardType
import sh.amux.design.AmuxTheme
import sh.amux.design.Space
import sh.amux.design.thumbTarget
import sh.amux.values.AskBody
import sh.amux.values.AskCard
import sh.amux.values.Choice
import sh.amux.values.ChoiceOutcome
import sh.amux.values.FieldProblem
import sh.amux.values.FormField
import sh.amux.values.FormFieldKind
import sh.amux.values.FormProblem
import sh.amux.values.FormValue
import sh.amux.words.ChatWords
import kotlin.math.floor

/**
 * Pure Kotlin schema validation matching `ui_view::form_problems` so [AskCardView]
 * validates forms offline in tests and storybook and can also be overridden by
 * a bridge-backed validator.
 */
fun validateFormFields(
    fields: List<FormField>,
    values: List<FormValue>,
): List<FormProblem> {
    val problems = mutableListOf<FormProblem>()
    fields.forEachIndexed { index, field ->
        val value = values.getOrNull(index) ?: field.initial
        val problem = validateSingleField(field, value)
        if (problem != null) {
            problems += FormProblem(field = index, problem = problem)
        }
    }
    return problems
}

private fun validateSingleField(field: FormField, value: FormValue): FieldProblem? =
    when (val kind = field.kind) {
        is FormFieldKind.Text -> {
            val trimmed = (value as? FormValue.Text)?.value?.trim().orEmpty()
            val minLen = kind.minLength
            val maxLen = kind.maxLength
            when {
                trimmed.isEmpty() -> if (field.required) FieldProblem.Required else null
                minLen != null && trimmed.length < minLen -> FieldProblem.TooShort(minLen)
                maxLen != null && trimmed.length > maxLen -> FieldProblem.TooLong(maxLen)
                else -> null
            }
        }
        is FormFieldKind.Number -> {
            val trimmed = (value as? FormValue.Text)?.value?.trim().orEmpty()
            val minVal = kind.minimum
            val maxVal = kind.maximum
            if (trimmed.isEmpty()) {
                if (field.required) FieldProblem.Required else null
            } else {
                val parsed = trimmed.toDoubleOrNull()
                when {
                    parsed == null || !parsed.isFinite() -> FieldProblem.NotANumber
                    kind.integer && floor(parsed) != parsed -> FieldProblem.NotWholeNumber
                    minVal != null && parsed < minVal -> FieldProblem.BelowMinimum(minVal)
                    maxVal != null && parsed > maxVal -> FieldProblem.AboveMaximum(maxVal)
                    else -> null
                }
            }
        }
        FormFieldKind.Toggle -> null
        is FormFieldKind.Choice -> {
            val chosen = (value as? FormValue.Choice)?.value
            if (chosen == null || chosen !in kind.options.indices) {
                if (field.required) FieldProblem.Required else null
            } else {
                null
            }
        }
        is FormFieldKind.Many -> {
            val picked = (value as? FormValue.Many)?.value.orEmpty()
                .filter { it in kind.options.indices }
                .distinct()
            val minItems = kind.minItems
            val maxItems = kind.maxItems
            when {
                picked.isEmpty() -> if (field.required) FieldProblem.Required else null
                minItems != null && picked.size < minItems -> FieldProblem.TooFew(minItems)
                maxItems != null && picked.size > maxItems -> FieldProblem.TooMany(maxItems)
                else -> null
            }
        }
    }

@Composable
internal fun FormAskSection(
    card: AskCard,
    form: AskBody.Form,
    keptValues: List<FormValue>?,
    onValuesChange: (List<FormValue>) -> Unit,
    validateForm: (List<FormField>, List<FormValue>) -> List<FormProblem>,
    onAction: (AskAction) -> Unit,
    modifier: Modifier = Modifier,
) {
    val initialValues = remember(card.key, form.fields) {
        if (keptValues != null && keptValues.size == form.fields.size) {
            keptValues
        } else {
            form.fields.map { it.initial }
        }
    }
    var values by remember(card.key) { mutableStateOf(initialValues) }
    var touched by remember(card.key) { mutableStateOf(emptySet<Int>()) }

    // The check goes to the core; it runs when the values change, not on every redraw.
    val problems = remember(form.fields, values) { validateForm(form.fields, values) }
    val problemsByField = problems.associateBy { it.field }

    Column(
        modifier = modifier,
        verticalArrangement = Arrangement.spacedBy(Space.m),
    ) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .weight(1f, fill = false)
                .verticalScroll(rememberScrollState()),
            verticalArrangement = Arrangement.spacedBy(Space.m),
        ) {
            if (form.message.isNotEmpty()) {
                AskWhyText(text = form.message)
            }
            form.fields.forEachIndexed { index, field ->
                val currentValue = values.getOrElse(index) { field.initial }
                val rawProblem = problemsByField[index]?.problem
                val shownProblem = when {
                    rawProblem == null -> null
                    rawProblem != FieldProblem.Required -> rawProblem
                    index in touched -> rawProblem
                    else -> null
                }
                FormFieldControl(
                    field = field,
                    value = currentValue,
                    problem = shownProblem,
                    onValueChange = { updated ->
                        val next = values.toMutableList()
                        while (next.size <= index) next.add(form.fields[next.size].initial)
                        next[index] = updated
                        values = next
                        touched = touched + index
                        onValuesChange(next)
                    },
                )
            }
        }

        val submitIndex = card.choices.indexOfFirst { it.outcome == ChoiceOutcome.Submit }.takeIf { it >= 0 }
        val declineIndex = card.choices.indexOfFirst { it.outcome != ChoiceOutcome.Submit }.takeIf { it >= 0 }

        AskButtonPair(
            primary = submitIndex?.let {
                {
                    AmuxActionButton(
                        label = stringResource(R.string.ask_form_submit),
                        kind = ActionButtonKind.Primary,
                        fillWidth = true,
                        enabled = problems.isEmpty(),
                        onClick = { onAction(AskAction.Submit(values, askKey = card.key)) },
                        modifier = Modifier.testTag(AskReviewTags.ASK_SUBMIT),
                    )
                }
            },
            secondary = declineIndex?.let { idx ->
                {
                    AmuxActionButton(
                        label = ChatWords.choice(card.choices[idx]),
                        kind = ActionButtonKind.Outline,
                        fillWidth = true,
                        onClick = { onAction(AskAction.Choose(idx, null, askKey = card.key)) },
                        modifier = Modifier.testTag(AskReviewTags.askChoice(idx)),
                    )
                }
            },
        )
    }
}

@Composable
private fun FormFieldControl(
    field: FormField,
    value: FormValue,
    problem: FieldProblem?,
    onValueChange: (FormValue) -> Unit,
) {
    val colors = AmuxTheme.colors
    val fieldLabel = if (field.required) {
        stringResource(R.string.ask_form_required_label, field.title)
    } else {
        field.title
    }

    Column(
        modifier = Modifier.fillMaxWidth(),
        verticalArrangement = Arrangement.spacedBy(Space.xs),
    ) {
        when (val kind = field.kind) {
            is FormFieldKind.Text, is FormFieldKind.Number -> {
                Text(
                    text = fieldLabel,
                    style = AmuxTheme.typography.detail,
                    color = colors.inkMuted,
                )
                if (field.description.isNotEmpty()) {
                    Text(
                        text = field.description,
                        style = AmuxTheme.typography.caption,
                        color = colors.inkFaint,
                    )
                }
                val text = (value as? FormValue.Text)?.value.orEmpty()
                OutlinedTextField(
                    value = text,
                    onValueChange = { onValueChange(FormValue.Text(it)) },
                    placeholder = {
                        Text(
                            text = field.title,
                            style = AmuxTheme.typography.body,
                            color = colors.inkFaint,
                        )
                    },
                    textStyle = AmuxTheme.typography.body.copy(color = colors.ink),
                    singleLine = true,
                    isError = problem != null,
                    keyboardOptions = KeyboardOptions(
                        keyboardType = if (kind is FormFieldKind.Number && (kind.minimum ?: -1.0) >= 0.0) {
                            KeyboardType.Decimal
                        } else {
                            KeyboardType.Text
                        },
                    ),
                    shape = RoundedCornerShape(AmuxTheme.metrics.controlRadius),
                    colors = amuxTextFieldColors(),
                    modifier = Modifier
                        .fillMaxWidth()
                        .semantics { contentDescription = fieldLabel }
                        .testTag(AskReviewTags.askField(field.name)),
                )
            }
            FormFieldKind.Toggle -> {
                val checked = (value as? FormValue.Toggle)?.value ?: false
                AmuxSwitchRow(
                    checked = checked,
                    onCheckedChange = { onValueChange(FormValue.Toggle(it)) },
                    modifier = Modifier
                        .fillMaxWidth()
                        .clip(RoundedCornerShape(AmuxTheme.metrics.controlRadius))
                        .testTag(AskReviewTags.askField(field.name)),
                    contentPadding = PaddingValues(vertical = Space.xs),
                    colors = amuxSwitchColors(),
                    spacing = Space.s,
                ) {
                    Column(modifier = Modifier.weight(1f)) {
                        Text(
                            text = field.title.ifEmpty { field.name },
                            style = AmuxTheme.typography.body,
                            color = colors.ink,
                        )
                        if (field.description.isNotEmpty()) {
                            Text(
                                text = field.description,
                                style = AmuxTheme.typography.caption,
                                color = colors.inkMuted,
                            )
                        }
                    }
                }
            }
            is FormFieldKind.Choice -> {
                var expanded by remember(field.name) { mutableStateOf(false) }
                val chosenIndex = (value as? FormValue.Choice)?.value
                val chosenLabel = chosenIndex?.let { kind.options.getOrNull(it) }
                    ?: stringResource(R.string.ask_form_choose)

                Text(
                    text = fieldLabel,
                    style = AmuxTheme.typography.detail,
                    color = colors.inkMuted,
                )
                if (field.description.isNotEmpty()) {
                    Text(
                        text = field.description,
                        style = AmuxTheme.typography.caption,
                        color = colors.inkFaint,
                    )
                }
                Box(modifier = Modifier.fillMaxWidth()) {
                    Surface(
                        modifier = Modifier
                            .fillMaxWidth()
                            .clip(RoundedCornerShape(AmuxTheme.metrics.controlRadius))
                            .clickable(role = Role.DropdownList) { expanded = true }
                            .thumbTarget()
                            .testTag(AskReviewTags.askField(field.name)),
                        shape = RoundedCornerShape(AmuxTheme.metrics.controlRadius),
                        color = colors.sunken,
                        border = BorderStroke(AmuxTheme.metrics.hairline, colors.hairline),
                    ) {
                        Row(
                            modifier = Modifier
                                .fillMaxWidth()
                                .padding(horizontal = Space.m, vertical = Space.m),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Text(
                                text = chosenLabel,
                                style = AmuxTheme.typography.body,
                                color = if (chosenIndex != null) colors.ink else colors.inkMuted,
                                modifier = Modifier.weight(1f),
                            )
                            Icon(
                                imageVector = Icons.Outlined.KeyboardArrowDown,
                                contentDescription = null,
                                tint = colors.inkMuted,
                                modifier = Modifier.size(IconSize.Control),
                            )
                        }
                    }
                    DropdownMenu(
                        expanded = expanded,
                        onDismissRequest = { expanded = false },
                        containerColor = colors.raised,
                    ) {
                        kind.options.forEachIndexed { optionIndex, option ->
                            DropdownMenuItem(
                                text = {
                                    Text(
                                        text = option,
                                        style = AmuxTheme.typography.body,
                                        color = colors.ink,
                                    )
                                },
                                onClick = {
                                    expanded = false
                                    onValueChange(FormValue.Choice(optionIndex))
                                },
                                modifier = Modifier
                                    .thumbTarget()
                                    .testTag(AskReviewTags.askFieldChoice(field.name, optionIndex)),
                            )
                        }
                    }
                }
            }
            is FormFieldKind.Many -> {
                val picked = (value as? FormValue.Many)?.value.orEmpty().toSet()
                Text(
                    text = fieldLabel,
                    style = AmuxTheme.typography.detail,
                    color = colors.inkMuted,
                )
                if (field.description.isNotEmpty()) {
                    Text(
                        text = field.description,
                        style = AmuxTheme.typography.caption,
                        color = colors.inkFaint,
                    )
                }
                Column(
                    modifier = Modifier
                        .fillMaxWidth()
                        .testTag(AskReviewTags.askField(field.name)),
                    verticalArrangement = Arrangement.spacedBy(Space.xs),
                ) {
                    kind.options.forEachIndexed { optionIndex, option ->
                        val isChecked = optionIndex in picked
                        Row(
                            modifier = Modifier
                                .fillMaxWidth()
                                .clip(RoundedCornerShape(AmuxTheme.metrics.controlRadius))
                                .clickable(role = Role.Checkbox) {
                                    val next = if (isChecked) picked - optionIndex else picked + optionIndex
                                    onValueChange(FormValue.Many(next.sorted()))
                                }
                                .thumbTarget()
                                .padding(horizontal = Space.xs)
                                .testTag(AskReviewTags.askFieldChoice(field.name, optionIndex)),
                            verticalAlignment = Alignment.CenterVertically,
                            horizontalArrangement = Arrangement.spacedBy(Space.s),
                        ) {
                            Checkbox(
                                checked = isChecked,
                                onCheckedChange = { checked ->
                                    val next = if (checked) picked + optionIndex else picked - optionIndex
                                    onValueChange(FormValue.Many(next.sorted()))
                                },
                                colors = CheckboxDefaults.colors(
                                    checkedColor = colors.ink,
                                    uncheckedColor = colors.inkMuted,
                                    checkmarkColor = colors.ground,
                                ),
                            )
                            Text(
                                text = option,
                                style = AmuxTheme.typography.body,
                                color = colors.ink,
                            )
                        }
                    }
                }
            }
        }

        if (problem != null) {
            Text(
                text = ChatWords.fieldProblem(problem),
                style = AmuxTheme.typography.detail,
                color = colors.accent,
                modifier = Modifier.testTag(AskReviewTags.askFieldProblem(field.name)),
            )
        }
    }
}
